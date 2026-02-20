"""Device hub and transport registry for Cast message routing."""

from __future__ import annotations

from dataclasses import dataclass, field
from threading import RLock
from typing import TYPE_CHECKING, Any, Protocol, override
from uuid import uuid4

from castvibe import _namespace as ns
from castvibe._log import get_logger
from castvibe._models import (
    ApplicationStatus,
    CastNamespace,
    ReceiverStatus,
    ReceiverStatusResponse,
    Volume,
)

if TYPE_CHECKING:
    from castvibe._connection import Connection
    from castvibe._proto.cast_channel_pb2 import CastMessage

log = get_logger("device")


class TransportHandler(Protocol):
    """Handler for messages addressed to a specific transport ID."""

    async def handle_message(self, connection: Connection, msg: CastMessage) -> None:
        """Handle an incoming Cast message for this transport."""
        ...


class Provider(Protocol):
    """Minimal provider interface required by the device hub."""

    def display_name(self) -> str:
        """Human-readable app/provider name shown in receiver status."""
        ...

    def namespaces(self) -> frozenset[str]:
        """Namespaces used by this provider session."""
        ...


@dataclass(slots=True, frozen=True)
class LaunchCredentials:
    """Credentials supplied with a LAUNCH request."""

    credentials: str | None = None
    credentials_type: str | None = None


@dataclass(slots=True)
class ReceiverConfig:
    """Identity/configuration fields used by the device hub."""

    friendly_name: str
    device_model: str = "Chromecast"
    device_id: str = field(default_factory=lambda: str(uuid4()))
    ssdp_udn: str = ""

    def __post_init__(self) -> None:
        if not self.ssdp_udn:
            self.ssdp_udn = self.device_id


@dataclass(slots=True)
class Subscription:
    """A sender subscription to a transport on a specific connection."""

    connection: Connection
    sender_id: str


@dataclass(slots=True)
class Transport:
    """Registered transport and all senders subscribed to it."""

    handler: TransportHandler
    subscriptions: list[Subscription] = field(default_factory=list)


@dataclass(slots=True)
class AppSession(TransportHandler):
    """Active application session registered as a Cast transport."""

    app_id: str
    display_name: str
    session_id: str
    transport_id: str
    provider: Provider
    credentials: LaunchCredentials
    namespaces: tuple[str, ...]
    status_text: str = ""

    @override
    async def handle_message(self, connection: Connection, msg: CastMessage) -> None:
        """Placeholder app message handler.

        Provider callback wiring is added in a later issue.
        """
        _ = connection
        log.debug(
            "session %s ignoring message on %s",
            self.session_id,
            msg.namespace,
        )


class Device:
    """Central hub for Cast transport registration, subscriptions, and routing."""

    __slots__ = (
        "_lock",
        "_subscriptions",
        "_transport_counter",
        "config",
        "sessions",
        "transports",
        "volume",
    )

    def __init__(self, config: ReceiverConfig) -> None:
        self._lock = RLock()
        self.config = config
        self.transports: dict[str, Transport] = {}
        self.sessions: dict[str, AppSession] = {}
        self._subscriptions: dict[tuple[Connection, str], str] = {}
        self._transport_counter = 1
        self.volume = Volume(
            level=1.0,
            muted=False,
            control_type="attenuation",
            step_interval=0.05,
        )

    # ------------------------------------------------------------------
    # Transport management
    # ------------------------------------------------------------------

    def register_transport(self, transport_id: str, handler: TransportHandler) -> None:
        """Register or replace a transport handler."""
        with self._lock:
            if transport_id in self.transports:
                self.unregister_transport(transport_id)
            self.transports[transport_id] = Transport(handler=handler)

    def unregister_transport(self, transport_id: str) -> None:
        """Unregister a transport and remove all associated subscriptions."""
        with self._lock:
            if transport_id not in self.transports:
                return

            del self.transports[transport_id]
            self._subscriptions = {
                key: value
                for key, value in self._subscriptions.items()
                if value != transport_id
            }

    # ------------------------------------------------------------------
    # Subscription management
    # ------------------------------------------------------------------

    def add_subscription(
        self,
        connection: Connection,
        sender_id: str,
        transport_id: str,
    ) -> None:
        """Subscribe *(connection, sender_id)* to a transport."""
        with self._lock:
            transport = self.transports.get(transport_id)
            if transport is None:
                log.warning(
                    "attempted subscription to unknown transport %s",
                    transport_id,
                )
                return

            key = (connection, sender_id)
            current_transport = self._subscriptions.get(key)
            if current_transport == transport_id:
                return

            if current_transport is not None:
                self.remove_subscription(connection, sender_id)

            self._subscriptions[key] = transport_id
            transport.subscriptions.append(
                Subscription(connection=connection, sender_id=sender_id)
            )

    def remove_subscription(self, connection: Connection, sender_id: str) -> None:
        """Remove subscription for a sender on a connection."""
        with self._lock:
            key = (connection, sender_id)
            transport_id = self._subscriptions.pop(key, None)
            if transport_id is None:
                return

            transport = self.transports.get(transport_id)
            if transport is None:
                return

            transport.subscriptions = [
                sub
                for sub in transport.subscriptions
                if not (sub.connection is connection and sub.sender_id == sender_id)
            ]

    def remove_all_subscriptions(self, connection: Connection) -> None:
        """Remove every subscription belonging to *connection*."""
        with self._lock:
            keys = [key for key in self._subscriptions if key[0] is connection]
            for _, sender_id in keys:
                self.remove_subscription(connection, sender_id)

    # ------------------------------------------------------------------
    # Message routing
    # ------------------------------------------------------------------

    async def route_message(self, connection: Connection, msg: CastMessage) -> None:
        """Route a message to the transport named in ``destination_id``."""
        with self._lock:
            transport = self.transports.get(msg.destination_id)
        if transport is None:
            log.warning(
                "unknown destination transport %s (namespace=%s)",
                msg.destination_id,
                msg.namespace,
            )
            return
        await transport.handler.handle_message(connection, msg)

    async def send_to_sender(
        self,
        connection: Connection,
        source_id: str,
        dest_id: str,
        namespace: str,
        data: dict[str, Any],
    ) -> None:
        """Send a JSON message to one sender on one connection."""
        await connection.send_json(
            source_id=source_id,
            dest_id=dest_id,
            namespace=namespace,
            data=data,
        )

    async def broadcast(
        self,
        source_id: str,
        namespace: str,
        data: dict[str, Any],
    ) -> None:
        """Broadcast a JSON message to all subscribers of ``source_id``."""
        with self._lock:
            transport = self.transports.get(source_id)
            if transport is None:
                log.warning("attempted broadcast from unknown transport %s", source_id)
                return

            connections = {sub.connection for sub in transport.subscriptions}
        for connection in connections:
            await connection.send_json(
                source_id=source_id,
                dest_id="*",
                namespace=namespace,
                data=data,
            )

    # ------------------------------------------------------------------
    # Session lifecycle
    # ------------------------------------------------------------------

    def start_session(
        self,
        app_id: str,
        provider: Provider,
        credentials: LaunchCredentials,
    ) -> AppSession:
        """Create and register an app session transport."""
        with self._lock:
            transport_id = f"pid-{self._transport_counter}"
            self._transport_counter += 1
            session_id = str(uuid4())

            provider_namespaces = sorted(
                namespace
                for namespace in provider.namespaces()
                if namespace != ns.MEDIA
            )
            provider_namespaces.append(ns.MEDIA)

            session = AppSession(
                app_id=app_id,
                display_name=provider.display_name(),
                session_id=session_id,
                transport_id=transport_id,
                provider=provider,
                credentials=credentials,
                namespaces=tuple(provider_namespaces),
            )
            self.sessions[session_id] = session
            self.register_transport(transport_id, session)
            return session

    def stop_session(self, session_id: str) -> None:
        """Stop and unregister a running app session."""
        with self._lock:
            session = self.sessions.pop(session_id, None)
            if session is None:
                return
            self.unregister_transport(session.transport_id)

    def snapshot_receiver_state(self) -> tuple[list[AppSession], Volume]:
        """Return atomic snapshots used for RECEIVER_STATUS generation."""
        with self._lock:
            sessions = list(self.sessions.values())
            volume = self.volume.model_copy(deep=True)
        return sessions, volume


def build_receiver_status(
    device: Device, request_id: int = 0
) -> ReceiverStatusResponse:
    """Build a ``RECEIVER_STATUS`` response from current device state."""
    sessions, volume = device.snapshot_receiver_state()

    applications = [
        ApplicationStatus(
            app_id=session.app_id,
            app_type="WEB",
            display_name=session.display_name,
            is_idle_screen=False,
            launched_from_cloud=False,
            namespaces=[CastNamespace(name=name) for name in session.namespaces],
            sender_connected=True,
            session_id=session.session_id,
            status_text=session.status_text,
            transport_id=session.transport_id,
            universal_app_id=session.app_id,
        )
        for session in sessions
    ]

    status = ReceiverStatus(
        applications=applications,
        volume=volume,
        is_active_input=True,
        is_stand_by=False,
    )
    return ReceiverStatusResponse(request_id=request_id, status=status)


__all__ = [
    "AppSession",
    "Device",
    "LaunchCredentials",
    "Provider",
    "ReceiverConfig",
    "Subscription",
    "Transport",
    "TransportHandler",
    "build_receiver_status",
]
