"""Tests for the central device hub."""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any, cast

from castvibe import _namespace as ns
from castvibe._device import Device, LaunchCredentials, ReceiverConfig
from tests.conftest import make_cast_message

if TYPE_CHECKING:
    from castvibe._connection import Connection
    from castvibe._device import Provider
    from castvibe._proto.cast_channel_pb2 import CastMessage


class RecordingConnection:
    """Connection double that records outbound JSON messages."""

    def __init__(self) -> None:
        self.sent: list[tuple[str, str, str, dict[str, Any]]] = []

    async def send_json(
        self,
        source_id: str,
        dest_id: str,
        namespace: str,
        data: dict[str, Any],
    ) -> None:
        self.sent.append((source_id, dest_id, namespace, data))


@dataclass(slots=True)
class RecordingHandler:
    """Transport handler double that captures routed messages."""

    calls: list[tuple[Connection, CastMessage]] = field(default_factory=list)

    async def handle_message(self, connection: Connection, msg: CastMessage) -> None:
        self.calls.append((connection, msg))


class FakeProvider:
    """Minimal provider implementation used by session tests."""

    def __init__(self, display_name: str, namespaces: frozenset[str]) -> None:
        self._display_name = display_name
        self._namespaces = namespaces

    def display_name(self) -> str:
        return self._display_name

    def namespaces(self) -> frozenset[str]:
        return self._namespaces


def _as_connection(connection: RecordingConnection) -> Connection:
    return cast("Connection", cast("object", connection))


def _build_device() -> Device:
    return Device(
        ReceiverConfig(
            friendly_name="Living Room",
            device_model="Chromecast",
            device_id="device-1234",
        )
    )


class TestTransportManagement:
    def test_register_unregister_transport(self) -> None:
        device = _build_device()
        handler = RecordingHandler()

        device.register_transport("receiver-0", handler)
        assert "receiver-0" in device.transports

        device.unregister_transport("receiver-0")
        assert "receiver-0" not in device.transports


class TestSubscriptions:
    def test_add_remove_and_remove_all(self) -> None:
        device = _build_device()
        device.register_transport("receiver-0", RecordingHandler())

        conn1 = _as_connection(RecordingConnection())
        conn2 = _as_connection(RecordingConnection())

        device.add_subscription(conn1, "sender-1", "receiver-0")
        device.add_subscription(conn1, "sender-2", "receiver-0")
        device.add_subscription(conn2, "sender-3", "receiver-0")

        assert len(device.transports["receiver-0"].subscriptions) == 3

        device.remove_subscription(conn1, "sender-1")
        assert len(device.transports["receiver-0"].subscriptions) == 2

        device.remove_all_subscriptions(conn1)
        subscriptions = device.transports["receiver-0"].subscriptions
        assert len(subscriptions) == 1
        assert subscriptions[0].sender_id == "sender-3"


class TestRouting:
    async def test_route_dispatches_to_transport_handler(self) -> None:
        device = _build_device()
        handler = RecordingHandler()
        device.register_transport("receiver-0", handler)

        conn = _as_connection(RecordingConnection())
        msg = make_cast_message(
            destination="receiver-0",
            namespace=ns.RECEIVER,
            payload_utf8='{"type":"GET_STATUS","requestId":1}',
        )

        await device.route_message(conn, msg)

        assert len(handler.calls) == 1
        assert handler.calls[0][0] is conn
        assert handler.calls[0][1].destination_id == "receiver-0"

    async def test_route_unknown_destination_is_logged(self, caplog: Any) -> None:
        device = _build_device()
        conn = _as_connection(RecordingConnection())
        msg = make_cast_message(
            destination="missing-transport",
            namespace=ns.RECEIVER,
            payload_utf8='{"type":"GET_STATUS","requestId":1}',
        )

        with caplog.at_level("WARNING", logger="castvibe.device"):
            await device.route_message(conn, msg)

        assert "unknown destination transport" in caplog.text

    async def test_broadcast_sends_to_all_subscribed_connections(self) -> None:
        device = _build_device()
        device.register_transport("receiver-0", RecordingHandler())

        conn1 = RecordingConnection()
        conn2 = RecordingConnection()
        device.add_subscription(_as_connection(conn1), "sender-1", "receiver-0")
        device.add_subscription(_as_connection(conn2), "sender-2", "receiver-0")

        await device.broadcast(
            source_id="receiver-0",
            namespace=ns.RECEIVER,
            data={"type": "RECEIVER_STATUS"},
        )

        assert len(conn1.sent) == 1
        assert len(conn2.sent) == 1
        assert conn1.sent[0][0] == "receiver-0"
        assert conn1.sent[0][1] == "*"


class TestSessionLifecycle:
    def test_start_and_stop_session(self) -> None:
        device = _build_device()
        provider: Provider = FakeProvider(
            display_name="Viaplay",
            namespaces=frozenset({"urn:x-cast:tv.viaplay.chromecast"}),
        )

        session = device.start_session(
            app_id="6313CF39",
            provider=provider,
            credentials=LaunchCredentials(
                credentials="token", credentials_type="bearer"
            ),
        )

        assert session.session_id in device.sessions
        assert session.transport_id in device.transports
        assert ns.MEDIA in session.namespaces
        assert "urn:x-cast:tv.viaplay.chromecast" in session.namespaces

        device.stop_session(session.session_id)

        assert session.session_id not in device.sessions
        assert session.transport_id not in device.transports

    def test_transport_ids_are_sequential(self) -> None:
        device = _build_device()
        provider: Provider = FakeProvider(display_name="App", namespaces=frozenset())

        first = device.start_session("app-1", provider, LaunchCredentials())
        second = device.start_session("app-2", provider, LaunchCredentials())

        assert first.transport_id == "pid-1"
        assert second.transport_id == "pid-2"
