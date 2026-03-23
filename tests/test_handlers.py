"""Tests for platform namespace handlers."""

from __future__ import annotations

import json
from pathlib import Path
from typing import TYPE_CHECKING, Any, cast, override

from tests.conftest import make_cast_message
from vibecast._models import (
    DeviceInfoResponse,
    LaunchErrorResponse,
    LoadRequest,
    MultizoneStatusResponse,
    ReceiverStatusResponse,
    SetupResponse,
    StreamType,
)
from vibecast._runtime.device import Device, DeviceIdentity
from vibecast._runtime.handlers import PlatformHandler
from vibecast._transport import namespace as ns
from vibecast.app import AppMessageDisposition, AppProvider, LaunchCredentials
from vibecast.player import DefaultPlayer, PlaybackMedia, PlaybackStream

if TYPE_CHECKING:
    from collections.abc import Callable

    from httpx import AsyncClient

    from vibecast._transport.connection import Connection


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


class FakeApp(AppProvider):
    """Minimal app used for LAUNCH/STOP tests."""

    @override
    def app_ids(self) -> frozenset[str]:
        return frozenset({"6313CF39"})

    @override
    def display_name(self) -> str:
        return "Viaplay"

    @override
    def app_key(self) -> str:
        return "fake"

    @override
    def namespaces(self) -> frozenset[str]:
        return frozenset({"urn:x-cast:tv.viaplay.chromecast"})

    @override
    async def on_launch(self, session: Any, credentials: Any) -> None:
        _ = session
        _ = credentials

    @override
    async def on_message(
        self, session: Any, namespace: str, data: dict[str, Any]
    ) -> AppMessageDisposition:
        _ = session
        _ = namespace
        _ = data
        return AppMessageDisposition.HANDLED

    @override
    async def resolve_media(
        self,
        session: Any,
        load_request: LoadRequest,
    ) -> PlaybackMedia:
        _ = session
        _ = load_request
        return PlaybackMedia(
            session_id="session",
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


def _build_device(app_lookup: Callable[[str], AppProvider | None]) -> Device:
    device = Device(
        DeviceIdentity(
            friendly_name="Living Room",
            device_model="Chromecast",
            device_id="device-1234",
            ssdp_udn="device-1234",
        ),
        get_http_client=lambda: cast("AsyncClient", object()),
        data_dir=Path("/tmp/vibecast-tests"),
    )
    platform = PlatformHandler(
        device,
        player=DefaultPlayer(),
        player_bridge=None,
        app_lookup=app_lookup,
    )
    device.register_transport("receiver-0", platform)
    return device


async def _route(
    device: Device,
    connection: RecordingConnection,
    *,
    namespace: str,
    payload: dict[str, Any],
    source: str = "sender-0",
    destination: str = "receiver-0",
) -> None:
    msg = make_cast_message(
        source=source,
        destination=destination,
        namespace=namespace,
        payload_utf8=json.dumps(payload, separators=(",", ":")),
    )
    await device.route_message(_as_connection(connection), msg)


async def _connect_sender(device: Device, connection: RecordingConnection) -> None:
    await _route(
        device,
        connection,
        namespace=ns.CONNECTION,
        payload={"type": "CONNECT", "origin": {}},
    )


class TestPlatformNamespaces:
    async def test_connection_close_removes_subscription(self) -> None:
        device = _build_device(lambda _app_id: None)
        connection = RecordingConnection()
        await _connect_sender(device, connection)

        subscriptions = device.transports["receiver-0"].subscriptions
        assert len(subscriptions) == 1

        await _route(
            device,
            connection,
            namespace=ns.CONNECTION,
            payload={"type": "CLOSE"},
        )

        assert device.transports["receiver-0"].subscriptions == []

    async def test_get_status_returns_receiver_status(self) -> None:
        device = _build_device(lambda _app_id: None)
        connection = RecordingConnection()

        await _route(
            device,
            connection,
            namespace=ns.RECEIVER,
            payload={"type": "GET_STATUS", "requestId": 1},
        )

        assert len(connection.sent) == 1
        response = ReceiverStatusResponse.model_validate(connection.sent[0][3])
        assert response.request_id == 1
        assert response.status.is_active_input is True
        assert response.status.is_stand_by is False
        assert response.status.volume.control_type == "attenuation"
        assert response.status.volume.step_interval == 0.05

    async def test_get_status_includes_active_session(self) -> None:
        fake_app = FakeApp()
        device = _build_device(
            lambda app_id: fake_app if app_id == "6313CF39" else None
        )
        _ = device.start_session(
            "6313CF39",
            fake_app,
            LaunchCredentials(),
            player=DefaultPlayer(),
            player_bridge=None,
        )
        connection = RecordingConnection()

        await _route(
            device,
            connection,
            namespace=ns.RECEIVER,
            payload={"type": "GET_STATUS", "requestId": 2},
        )

        response = ReceiverStatusResponse.model_validate(connection.sent[0][3])
        assert len(response.status.applications) == 1
        app = response.status.applications[0]
        assert app.app_id == "6313CF39"
        assert app.display_name == "Viaplay"
        namespaces = {entry.name for entry in app.namespaces}
        assert ns.MEDIA in namespaces
        assert "urn:x-cast:tv.viaplay.chromecast" in namespaces

    async def test_get_app_availability_marks_all_available(self) -> None:
        device = _build_device(lambda _app_id: None)
        connection = RecordingConnection()

        await _route(
            device,
            connection,
            namespace=ns.RECEIVER,
            payload={
                "type": "GET_APP_AVAILABILITY",
                "requestId": 3,
                "appId": ["6313CF39", "CC1AD845"],
            },
        )

        data = connection.sent[0][3]
        assert data["type"] == "GET_APP_AVAILABILITY"
        assert data["availability"]["6313CF39"] == "APP_AVAILABLE"
        assert data["availability"]["CC1AD845"] == "APP_AVAILABLE"

    async def test_launch_creates_session_and_broadcasts_status(self) -> None:
        app = FakeApp()
        device = _build_device(lambda app_id: app if app_id == "6313CF39" else None)
        connection = RecordingConnection()
        await _connect_sender(device, connection)

        await _route(
            device,
            connection,
            namespace=ns.RECEIVER,
            payload={
                "type": "LAUNCH",
                "requestId": 4,
                "appId": "6313CF39",
            },
        )

        assert len(device.sessions) == 1
        assert len(connection.sent) == 1
        source_id, dest_id, namespace, data = connection.sent[0]
        assert source_id == "receiver-0"
        assert dest_id == "*"
        assert namespace == ns.RECEIVER
        response = ReceiverStatusResponse.model_validate(data)
        assert len(response.status.applications) == 1

    async def test_launch_replaces_existing_session(self) -> None:
        app = FakeApp()

        def lookup(app_id: str) -> AppProvider | None:
            return app if app_id in ("6313CF39", "95370A1C") else None

        device = _build_device(lookup)
        connection = RecordingConnection()
        await _connect_sender(device, connection)

        # Launch Viaplay.
        await _route(
            device,
            connection,
            namespace=ns.RECEIVER,
            payload={
                "type": "LAUNCH",
                "requestId": 10,
                "appId": "6313CF39",
            },
        )
        assert len(device.sessions) == 1
        old_session_id = next(iter(device.sessions))

        connection.sent.clear()

        # Launch SVT Play -- should replace the Viaplay session.
        await _route(
            device,
            connection,
            namespace=ns.RECEIVER,
            payload={
                "type": "LAUNCH",
                "requestId": 11,
                "appId": "95370A1C",
            },
        )

        assert len(device.sessions) == 1
        new_session_id = next(iter(device.sessions))
        assert new_session_id != old_session_id

        response = ReceiverStatusResponse.model_validate(connection.sent[0][3])
        assert len(response.status.applications) == 1
        assert response.status.applications[0].app_id == "95370A1C"

    async def test_launch_unavailable_app_returns_launch_error(self) -> None:
        device = _build_device(lambda _app_id: None)
        connection = RecordingConnection()

        await _route(
            device,
            connection,
            namespace=ns.RECEIVER,
            payload={
                "type": "LAUNCH",
                "requestId": 41,
                "appId": "UNKNOWN",
            },
        )

        assert len(connection.sent) == 1
        source_id, dest_id, namespace, data = connection.sent[0]
        assert source_id == "receiver-0"
        assert dest_id == "sender-0"
        assert namespace == ns.RECEIVER
        response = LaunchErrorResponse.model_validate(data)
        assert response.request_id == 41
        assert response.reason == "Application not available"

    async def test_stop_removes_session_and_broadcasts_status(self) -> None:
        app = FakeApp()
        device = _build_device(lambda app_id: app if app_id == "6313CF39" else None)
        connection = RecordingConnection()
        await _connect_sender(device, connection)

        await _route(
            device,
            connection,
            namespace=ns.RECEIVER,
            payload={
                "type": "LAUNCH",
                "requestId": 5,
                "appId": "6313CF39",
            },
        )

        session_id = next(iter(device.sessions))
        connection.sent.clear()

        await _route(
            device,
            connection,
            namespace=ns.RECEIVER,
            payload={
                "type": "STOP",
                "requestId": 6,
                "sessionId": session_id,
            },
        )

        assert len(device.sessions) == 0
        assert len(connection.sent) == 1
        response = ReceiverStatusResponse.model_validate(connection.sent[0][3])
        assert response.request_id == 6
        assert response.status.applications == []

    async def test_set_volume_updates_state_and_broadcasts_status(self) -> None:
        device = _build_device(lambda _app_id: None)
        connection = RecordingConnection()
        await _connect_sender(device, connection)

        await _route(
            device,
            connection,
            namespace=ns.RECEIVER,
            payload={
                "type": "SET_VOLUME",
                "requestId": 7,
                "volume": {"level": 0.4, "muted": True},
            },
        )

        assert device.volume.level == 0.4
        assert device.volume.muted is True
        response = ReceiverStatusResponse.model_validate(connection.sent[0][3])
        assert response.status.volume.level == 0.4
        assert response.status.volume.muted is True

    async def test_get_device_info_returns_receiver_metadata(self) -> None:
        device = _build_device(lambda _app_id: None)
        connection = RecordingConnection()

        await _route(
            device,
            connection,
            namespace=ns.DISCOVERY,
            payload={"type": "GET_DEVICE_INFO", "requestId": 8},
        )

        assert len(connection.sent) == 1
        response = DeviceInfoResponse.model_validate(connection.sent[0][3])
        assert response.request_id == 8
        assert response.device_id == "device-1234"
        assert response.friendly_name == "Living Room"

    async def test_multizone_get_status_returns_empty_status(self) -> None:
        device = _build_device(lambda _app_id: None)
        connection = RecordingConnection()

        await _route(
            device,
            connection,
            namespace=ns.MULTIZONE,
            payload={"type": "GET_STATUS", "requestId": 9},
        )

        assert len(connection.sent) == 1
        response = MultizoneStatusResponse.model_validate(connection.sent[0][3])
        assert response.request_id == 9
        assert response.status.devices == []
        assert response.status.is_multichannel is False

    async def test_setup_eureka_info_returns_ssdp_udn(self) -> None:
        device = _build_device(lambda _app_id: None)
        connection = RecordingConnection()

        await _route(
            device,
            connection,
            namespace=ns.SETUP,
            payload={"type": "eureka_info", "request_id": 10},
        )

        assert len(connection.sent) == 1
        response = SetupResponse.model_validate(connection.sent[0][3])
        assert response.type == "eureka_info"
        assert response.request_id == 10
        assert response.data.device_info.ssdp_udn == "device-1234"
