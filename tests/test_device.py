"""Tests for the central device hub."""

from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import Path
from typing import TYPE_CHECKING, Any, cast, override

from tests.conftest import make_cast_message
from vibecast import _namespace as ns
from vibecast._device import Device, DeviceIdentity, build_receiver_status
from vibecast._models import LoadRequest, StreamType
from vibecast.player import DefaultPlayer, PlaybackMedia, PlaybackStream
from vibecast.provider import LaunchCredentials, Provider

if TYPE_CHECKING:
    from httpx import AsyncClient

    from vibecast._connection import Connection
    from vibecast._proto.cast_channel_pb2 import CastMessage


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


class FailingConnection:
    async def send_json(
        self,
        source_id: str,
        dest_id: str,
        namespace: str,
        data: dict[str, Any],
    ) -> None:
        _ = source_id
        _ = dest_id
        _ = namespace
        _ = data
        msg = "Connection lost"
        raise ConnectionResetError(msg)


@dataclass(slots=True)
class RecordingHandler:
    """Transport handler double that captures routed messages."""

    calls: list[tuple[Connection, CastMessage]] = field(default_factory=list)

    async def handle_message(self, connection: Connection, msg: CastMessage) -> None:
        self.calls.append((connection, msg))


class FakeProvider(Provider):
    """Minimal provider implementation used by session tests."""

    def __init__(self, display_name: str, namespaces: frozenset[str]) -> None:
        self._display_name = display_name
        self._namespaces = namespaces
        self.stop_calls = 0

    @override
    def app_ids(self) -> frozenset[str]:
        return frozenset({"6313CF39"})

    @override
    def display_name(self) -> str:
        return self._display_name

    @override
    def namespaces(self) -> frozenset[str]:
        return self._namespaces

    @override
    async def on_launch(self, session: Any, credentials: Any) -> None:
        _ = session
        _ = credentials

    @override
    async def on_message(
        self, session: Any, namespace: str, data: dict[str, Any]
    ) -> None:
        _ = session
        _ = namespace
        _ = data

    @override
    async def on_stop(self, session: Any) -> None:
        _ = session
        self.stop_calls += 1

    @override
    async def resolve_media(
        self,
        session: Any,
        load_request: LoadRequest,
    ) -> PlaybackMedia:
        _ = session
        _ = load_request
        return PlaybackMedia(
            session_id="sess",
            streams=(
                PlaybackStream(
                    url="https://example.com/video.mpd",
                    content_type="application/dash+xml",
                ),
            ),
            stream_type=StreamType.BUFFERED,
        )


def _as_connection(connection: RecordingConnection) -> Connection:
    return cast("Connection", cast("object", connection))


def _build_device() -> Device:
    return Device(
        DeviceIdentity(
            friendly_name="Living Room",
            device_model="Chromecast",
            device_id="device-1234",
        ),
        get_http_client=lambda: cast("AsyncClient", object()),
        data_dir=Path("/tmp/vibecast-tests"),
    )


def _build_player() -> DefaultPlayer:
    return DefaultPlayer()


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

        _ = device.remove_subscription(conn1, "sender-1")
        assert len(device.transports["receiver-0"].subscriptions) == 2

        _ = device.remove_all_subscriptions(conn1)
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

        with caplog.at_level("WARNING", logger="vibecast.device"):
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

    async def test_broadcast_prunes_broken_connections(self) -> None:
        device = _build_device()
        device.register_transport("receiver-0", RecordingHandler())

        broken = cast("Connection", cast("object", FailingConnection()))
        device.add_subscription(broken, "sender-1", "receiver-0")

        await device.broadcast(
            source_id="receiver-0",
            namespace=ns.RECEIVER,
            data={"type": "RECEIVER_STATUS"},
        )

        assert device.transports["receiver-0"].subscriptions == []


class TestSessionLifecycle:
    def test_session_receiver_context_uses_receiver_managed_data_dir(self) -> None:
        device = _build_device()
        provider = FakeProvider(display_name="Viaplay", namespaces=frozenset())

        # New coordinator pipeline always wires a player.
        session = device.start_session(
            "6313CF39",
            provider,
            LaunchCredentials(),
            player=_build_player(),
            player_server=None,
        )
        assert session.receiver.friendly_name == "Living Room"
        assert session.receiver.device_model == "Chromecast"
        assert session.receiver.device_id == "device-1234"
        assert session.receiver.data_dir == Path("/tmp/vibecast-tests/providers/fake")
        assert session.receiver.data_dir.exists()
        assert "CrKey/1.56.500000" in session.receiver.user_agent
        assert "display_supported" in session.receiver.cast_device_capabilities
        assert session.receiver.display_width == 1920
        assert session.receiver.display_height == 1080

    async def test_start_and_stop_session(self) -> None:
        device = _build_device()
        provider = FakeProvider(
            display_name="Viaplay",
            namespaces=frozenset({"urn:x-cast:tv.viaplay.chromecast"}),
        )

        session = device.start_session(
            app_id="6313CF39",
            provider=provider,
            credentials=LaunchCredentials(
                credentials="token", credentials_type="bearer"
            ),
            player=_build_player(),
            player_server=None,
        )

        assert session.session_id in device.sessions
        assert session.transport_id in device.transports
        assert ns.MEDIA in session.namespaces
        assert "urn:x-cast:tv.viaplay.chromecast" in session.namespaces

        _ = await device.stop_session(session.session_id)

        assert session.session_id not in device.sessions
        assert session.transport_id not in device.transports

    def test_transport_id_equals_session_id(self) -> None:
        device = _build_device()
        provider = FakeProvider(display_name="App", namespaces=frozenset())

        session = device.start_session(
            "app-1",
            provider,
            LaunchCredentials(),
            player=_build_player(),
            player_server=None,
        )

        assert session.transport_id == session.session_id

    async def test_stop_orphaned_session_when_last_subscription_removed(self) -> None:
        device = _build_device()
        provider = FakeProvider(display_name="Viaplay", namespaces=frozenset())
        session = device.start_session(
            "6313CF39",
            provider,
            LaunchCredentials(),
            player=_build_player(),
            player_server=None,
        )

        conn = _as_connection(RecordingConnection())
        device.add_subscription(conn, "sender-1", session.transport_id)

        transport_id = device.remove_subscription(conn, "sender-1")
        assert transport_id == session.transport_id

        stopped = await device.stop_orphaned_sessions({session.transport_id})

        assert stopped == [session.session_id]
        assert session.session_id not in device.sessions
        assert provider.stop_calls == 1

    async def test_keeps_session_when_other_subscribers_remain(self) -> None:
        device = _build_device()
        provider = FakeProvider(display_name="Viaplay", namespaces=frozenset())
        session = device.start_session(
            "6313CF39",
            provider,
            LaunchCredentials(),
            player=_build_player(),
            player_server=None,
        )

        conn1 = _as_connection(RecordingConnection())
        conn2 = _as_connection(RecordingConnection())
        device.add_subscription(conn1, "sender-1", session.transport_id)
        device.add_subscription(conn2, "sender-2", session.transport_id)

        _ = device.remove_subscription(conn1, "sender-1")
        stopped = await device.stop_orphaned_sessions({session.transport_id})

        assert stopped == []
        assert session.session_id in device.sessions
        assert provider.stop_calls == 0

    def test_receiver_status_sender_connected_tracks_subscriptions(self) -> None:
        device = _build_device()
        provider = FakeProvider(display_name="Viaplay", namespaces=frozenset())
        session = device.start_session(
            "6313CF39",
            provider,
            LaunchCredentials(),
            player=_build_player(),
            player_server=None,
        )

        status = build_receiver_status(device)
        app = status.status.applications[0]
        assert app.sender_connected is False

        conn = _as_connection(RecordingConnection())
        device.add_subscription(conn, "sender-1", session.transport_id)

        status = build_receiver_status(device)
        app = status.status.applications[0]
        assert app.sender_connected is True
