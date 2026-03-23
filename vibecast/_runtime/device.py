"""Device hub and transport registry for Cast message routing.

``Device`` is the internal routing hub: it owns transports, subscriptions, and
sessions.  It does **not** manage networking or lifecycle — that is the job of
``CastReceiver`` (see ``vibecast.receiver``), which wires Device to the TLS
listener, mDNS advertisement, and PlayerBridge.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any, Protocol, override
from uuid import uuid4

from pydantic import ValidationError

import vibecast._transport.namespace as ns
from vibecast._config import CastConfig, cast_device_capabilities_header
from vibecast._log import get_logger
from vibecast._models import (
    ConnectRequest,
    MediaInvalidRequestResponse,
    Volume,
    connection_message_adapter,
    media_request_adapter,
)
from vibecast._playback.coordinator import PlaybackCoordinator
from vibecast._runtime.receiver_status import build_receiver_status
from vibecast._util import extract_request_id, parse_json_payload
from vibecast.app import (
    AppContext,
    AppMessageDisposition,
    AppProvider,
    LaunchCredentials,
    ReceiverContext,
)

if TYPE_CHECKING:
    from collections.abc import Callable
    from pathlib import Path

    from httpx import AsyncClient

    from vibecast._playback.player_bridge import PlayerBridge
    from vibecast._proto.cast_channel_pb2 import CastMessage
    from vibecast._transport.connection import Connection
    from vibecast.player import Player

log = get_logger("device")
_DEFAULT_CAST_CONFIG = CastConfig()
_DEFAULT_CAST_DEVICE_CAPABILITIES = cast_device_capabilities_header(
    _DEFAULT_CAST_CONFIG.device_capabilities
)


class TransportHandler(Protocol):
    """Handler for messages addressed to a specific transport ID."""

    async def handle_message(self, connection: Connection, msg: CastMessage) -> None:
        """Handle an incoming Cast message for this transport."""
        ...


@dataclass(slots=True)
class DeviceIdentity:
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

    device: Device
    app_id: str
    display_name: str
    session_id: str
    transport_id: str
    app: AppProvider
    receiver: ReceiverContext
    credentials: LaunchCredentials
    namespaces: tuple[str, ...]
    coordinator: PlaybackCoordinator | None = None
    icon_url: str | None = None
    status_text: str = ""

    async def on_launch(self, connection: Connection, sender_id: str) -> None:
        """Invoke app launch callback."""
        context = self._build_context(connection=connection, sender_id=sender_id)
        await self.app.on_launch(context, self.credentials)

    async def on_stop(self) -> None:
        """Invoke app stop callback."""
        if self.coordinator is not None:
            await self.coordinator.close()
        context = self._build_context(connection=None, sender_id=None)
        await self.app.on_stop(context)

    @override
    async def handle_message(self, connection: Connection, msg: CastMessage) -> None:
        """Route app-transport messages to the app callbacks."""
        payload = parse_json_payload(msg)
        if payload is None:
            return

        if msg.namespace == ns.CONNECTION:
            await self._handle_connection_message(connection, msg.source_id, payload)
            return

        context = self._build_context(connection=connection, sender_id=msg.source_id)

        if msg.namespace == ns.MEDIA:
            await self._handle_media_message(connection, msg.source_id, payload)
            return

        if msg.namespace in self.namespaces and msg.namespace != ns.MEDIA:
            disposition = await self.app.on_message(
                context,
                msg.namespace,
                payload,
            )
            if disposition != AppMessageDisposition.HANDLED:
                raw_type = payload.get("type")
                if isinstance(raw_type, str):
                    log.debug(
                        "session %s app %s left message unhandled namespace=%s type=%s",
                        self.session_id,
                        self.app.app_key(),
                        msg.namespace,
                        raw_type,
                    )
                else:
                    log.debug(
                        "session %s app %s left message unhandled namespace=%s",
                        self.session_id,
                        self.app.app_key(),
                        msg.namespace,
                    )
            return

        log.warning(
            "session %s received unsupported namespace %s",
            self.session_id,
            msg.namespace,
        )

    async def _handle_connection_message(
        self,
        connection: Connection,
        sender_id: str,
        payload: dict[str, Any],
    ) -> None:
        try:
            request = connection_message_adapter.validate_python(payload)
        except ValidationError:
            log.warning("invalid app connection payload", exc_info=True)
            return

        if isinstance(request, ConnectRequest):
            self.device.add_subscription(connection, sender_id, self.transport_id)
            if self.coordinator is not None:
                await self.coordinator.send_current_status(connection, sender_id)
            context = self._build_context(connection=connection, sender_id=sender_id)
            await self.app.on_sender_connected(context, sender_id)
            return

        _ = self.device.remove_subscription(connection, sender_id)

    async def _handle_media_message(
        self,
        connection: Connection,
        sender_id: str,
        payload: dict[str, Any],
    ) -> None:
        try:
            request = media_request_adapter.validate_python(payload)
        except ValidationError:
            response = MediaInvalidRequestResponse(
                request_id=extract_request_id(payload),
                reason="Invalid media request",
            )
            await self.device.send_to_sender(
                connection=connection,
                source_id=self.transport_id,
                dest_id=sender_id,
                namespace=ns.MEDIA,
                data=response.model_dump(exclude_none=True),
            )
            return

        coordinator = self.coordinator
        if coordinator is None:
            response = MediaInvalidRequestResponse(
                request_id=extract_request_id(payload),
                reason="No playback coordinator",
            )
            await self.device.send_to_sender(
                connection=connection,
                source_id=self.transport_id,
                dest_id=sender_id,
                namespace=ns.MEDIA,
                data=response.model_dump(exclude_none=True),
            )
            return

        await coordinator.handle_media_message(connection, sender_id, request)

    def _build_context(
        self,
        *,
        connection: Connection | None,
        sender_id: str | None,
    ) -> AppContext:
        async def send_custom(namespace: str, data: dict[str, Any]) -> None:
            if connection is None or sender_id is None:
                await self.device.broadcast(
                    source_id=self.transport_id,
                    namespace=namespace,
                    data=data,
                )
                return
            await self.device.send_to_sender(
                connection=connection,
                source_id=self.transport_id,
                dest_id=sender_id,
                namespace=namespace,
                data=data,
            )

        async def broadcast_custom(namespace: str, data: dict[str, Any]) -> None:
            await self.device.broadcast(
                source_id=self.transport_id,
                namespace=namespace,
                data=data,
            )

        return AppContext(
            session_id=self.session_id,
            transport_id=self.transport_id,
            app_id=self.app_id,
            http_client=self.device.http_client,
            receiver=self.receiver,
            send_custom=send_custom,
            broadcast_custom=broadcast_custom,
        )

    def create_app_context(self) -> AppContext:
        """Build an app context for internal service callbacks."""
        return self._build_context(connection=None, sender_id=None)


class Device:
    """Central hub for Cast transport registration, subscriptions, and routing."""

    __slots__ = (
        "_data_dir",
        "_get_http_client",
        "_cast_device_capabilities",
        "_display_height",
        "_display_width",
        "_user_agent",
        "_subscriptions",
        "config",
        "sessions",
        "transports",
        "volume",
    )

    def __init__(
        self,
        config: DeviceIdentity,
        *,
        get_http_client: Callable[[], AsyncClient],
        data_dir: Path,
        volume_level: float = 1.0,
        volume_muted: bool = False,
        volume_step_interval: float = 0.05,
        user_agent: str = _DEFAULT_CAST_CONFIG.user_agent,
        cast_device_capabilities: str = _DEFAULT_CAST_DEVICE_CAPABILITIES,
        display_width: int = 1920,
        display_height: int = 1080,
    ) -> None:
        self.config = config
        self._get_http_client = get_http_client
        self._data_dir = data_dir
        self._user_agent = user_agent
        self._cast_device_capabilities = cast_device_capabilities
        self._display_width = display_width
        self._display_height = display_height
        self._data_dir.mkdir(parents=True, exist_ok=True)
        self.transports: dict[str, Transport] = {}
        self.sessions: dict[str, AppSession] = {}
        self._subscriptions: dict[tuple[Connection, str], str] = {}
        self.volume = Volume(
            level=volume_level,
            muted=volume_muted,
            control_type="attenuation",
            step_interval=volume_step_interval,
        )

    @property
    def http_client(self) -> AsyncClient:
        return self._get_http_client()

    # ------------------------------------------------------------------
    # Transport management
    # ------------------------------------------------------------------

    def register_transport(self, transport_id: str, handler: TransportHandler) -> None:
        """Register or replace a transport handler."""
        if transport_id in self.transports:
            self.unregister_transport(transport_id)
        self.transports[transport_id] = Transport(handler=handler)

    def unregister_transport(self, transport_id: str) -> None:
        """Unregister a transport and remove all associated subscriptions."""
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
            _ = self.remove_subscription(connection, sender_id)

        self._subscriptions[key] = transport_id
        transport.subscriptions.append(
            Subscription(connection=connection, sender_id=sender_id)
        )

    def remove_subscription(self, connection: Connection, sender_id: str) -> str | None:
        """Remove subscription for a sender on a connection.

        Returns the transport ID the sender was subscribed to, or ``None``
        when no subscription existed.
        """
        key = (connection, sender_id)
        transport_id = self._subscriptions.pop(key, None)
        if transport_id is None:
            return None

        transport = self.transports.get(transport_id)
        if transport is None:
            return transport_id

        transport.subscriptions = [
            sub
            for sub in transport.subscriptions
            if not (sub.connection is connection and sub.sender_id == sender_id)
        ]
        return transport_id

    def remove_all_subscriptions(self, connection: Connection) -> set[str]:
        """Remove every subscription belonging to *connection*.

        Returns all transport IDs that were affected.
        """
        keys = [key for key in self._subscriptions if key[0] is connection]
        transport_ids: set[str] = set()
        for _, sender_id in keys:
            transport_id = self.remove_subscription(connection, sender_id)
            if transport_id is not None:
                transport_ids.add(transport_id)
        return transport_ids

    # ------------------------------------------------------------------
    # Message routing
    # ------------------------------------------------------------------

    async def route_message(self, connection: Connection, msg: CastMessage) -> None:
        """Route a message to the transport named in ``destination_id``."""
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
        try:
            await connection.send_json(
                source_id=source_id,
                dest_id=dest_id,
                namespace=namespace,
                data=data,
            )
        except (ConnectionResetError, BrokenPipeError, OSError, RuntimeError):
            _ = self.remove_all_subscriptions(connection)

    async def broadcast(
        self,
        source_id: str,
        namespace: str,
        data: dict[str, Any],
    ) -> None:
        """Broadcast a JSON message to all subscribers of ``source_id``."""
        transport = self.transports.get(source_id)
        if transport is None:
            log.warning("attempted broadcast from unknown transport %s", source_id)
            return

        connections = {sub.connection for sub in transport.subscriptions}
        for connection in connections:
            try:
                await connection.send_json(
                    source_id=source_id,
                    dest_id="*",
                    namespace=namespace,
                    data=data,
                )
            except (ConnectionResetError, BrokenPipeError, OSError, RuntimeError):
                _ = self.remove_all_subscriptions(connection)

    # ------------------------------------------------------------------
    # Session lifecycle
    # ------------------------------------------------------------------

    def start_session(
        self,
        app_id: str,
        app: AppProvider,
        credentials: LaunchCredentials,
        *,
        player: Player,
        player_bridge: PlayerBridge | None,
    ) -> AppSession:
        """Create and register an app session transport."""
        session_id = str(uuid4())
        transport_id = session_id

        app_namespaces = sorted(
            namespace for namespace in app.namespaces() if namespace != ns.MEDIA
        )
        app_namespaces.append(ns.MEDIA)

        app_data_dir = self._data_dir / "apps" / app.app_key()
        app_data_dir.mkdir(parents=True, exist_ok=True)

        session = AppSession(
            device=self,
            app_id=app_id,
            display_name=app.display_name(),
            session_id=session_id,
            transport_id=transport_id,
            app=app,
            receiver=ReceiverContext(
                friendly_name=self.config.friendly_name,
                device_model=self.config.device_model,
                device_id=self.config.device_id,
                data_dir=app_data_dir,
                user_agent=self._user_agent,
                cast_device_capabilities=self._cast_device_capabilities,
                display_width=self._display_width,
                display_height=self._display_height,
            ),
            credentials=credentials,
            namespaces=tuple(app_namespaces),
            icon_url=app.icon_url(),
            status_text=app.display_name(),
        )

        app_context = session.create_app_context()

        async def broadcast_fn(namespace: str, data: dict[str, Any]) -> None:
            await self.broadcast(
                source_id=transport_id,
                namespace=namespace,
                data=data,
            )

        async def send_fn(
            connection: Connection,
            sender_id: str,
            namespace: str,
            data: dict[str, Any],
        ) -> None:
            await self.send_to_sender(
                connection=connection,
                source_id=transport_id,
                dest_id=sender_id,
                namespace=namespace,
                data=data,
            )

        session.coordinator = PlaybackCoordinator(
            session_id=session_id,
            transport_id=transport_id,
            app=app,
            app_context=app_context,
            player=player,
            player_bridge=player_bridge,
            broadcast_fn=broadcast_fn,
            send_fn=send_fn,
            initial_volume=self.volume,
        )

        self.sessions[session_id] = session
        self.register_transport(transport_id, session)
        return session

    async def stop_session(self, session_id: str) -> AppSession | None:
        """Stop a running app session.

        Invokes the app ``on_stop`` callback *before* unregistering the
        transport so the app can still send/broadcast during teardown.
        """
        session = self.sessions.pop(session_id, None)
        if session is None:
            return None
        try:
            await session.on_stop()
        except Exception:
            log.warning("app on_stop failed for session %s", session_id, exc_info=True)
        self.unregister_transport(session.transport_id)
        return session

    async def stop_orphaned_sessions(
        self,
        transport_ids: set[str] | None = None,
    ) -> list[str]:
        """Stop app sessions that no longer have sender subscriptions.

        If *transport_ids* is provided, only sessions for those transports are
        evaluated.
        """
        stopped: list[str] = []

        for session_id, session in list(self.sessions.items()):
            if transport_ids is not None and session.transport_id not in transport_ids:
                continue

            transport = self.transports.get(session.transport_id)
            if transport is None or transport.subscriptions:
                continue

            _ = await self.stop_session(session_id)
            stopped.append(session_id)

        if stopped and "receiver-0" in self.transports:
            status = build_receiver_status(self)
            await self.broadcast(
                source_id="receiver-0",
                namespace=ns.RECEIVER,
                data=status.model_dump(exclude_none=True),
            )

        return stopped

    def session_ids(self) -> list[str]:
        """Return the current app session IDs."""
        return list(self.sessions.keys())

    def snapshot_receiver_state(self) -> tuple[list[AppSession], Volume]:
        """Return snapshots used for RECEIVER_STATUS generation."""
        sessions = list(self.sessions.values())
        volume = self.volume.model_copy(deep=True)
        return sessions, volume


__all__ = [
    "AppSession",
    "Device",
    "DeviceIdentity",
    "Subscription",
    "Transport",
    "TransportHandler",
]
