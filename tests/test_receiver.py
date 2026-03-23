"""Tests for the public CastReceiver API."""

from __future__ import annotations

import asyncio
import json
import struct
from dataclasses import replace
from typing import TYPE_CHECKING, Any, override

import pytest

from tests.conftest import make_cast_message
from vibecast._config import DeviceConfig, NetworkConfig, VibecastConfig
from vibecast._models import LoadRequest, StreamType
from vibecast._proto.cast_channel_pb2 import (
    AuthChallenge,
    CastMessage,
    DeviceAuthMessage,
)
from vibecast._transport import namespace as ns
from vibecast.app import (
    AppContext,
    AppMessageDisposition,
    AppProvider,
    LaunchCredentials,
)
from vibecast.player import PlaybackMedia, PlaybackStream
from vibecast.receiver import CastReceiver

if TYPE_CHECKING:
    from ssl import SSLContext

    from vibecast._security.certificate import CertificateBundle


def _frame(msg: CastMessage) -> bytes:
    payload = msg.SerializeToString()
    return struct.pack(">I", len(payload)) + payload


async def _read_framed(reader: asyncio.StreamReader) -> CastMessage:
    header = await asyncio.wait_for(reader.readexactly(4), timeout=5)
    (length,) = struct.unpack(">I", header)
    payload = await asyncio.wait_for(reader.readexactly(length), timeout=5)
    msg = CastMessage()
    _ = msg.ParseFromString(payload)
    return msg


class DummyAdvertisement:
    def __init__(
        self,
        friendly_name: str,
        device_model: str,
        device_id: str,
        port: int,
        cert_digest: str,
        app_ids: object = (),
    ) -> None:
        self.friendly_name = friendly_name
        self.device_model = device_model
        self.device_id = device_id
        self.port = port
        self.cert_digest = cert_digest
        self.app_ids = app_ids
        self.started = False
        self.service_name = (
            f"vibecast-{device_id.replace('-', '')}._googlecast._tcp.local."
        )
        self.parsed_addresses = ("127.0.0.1",)

    async def start(self) -> None:
        self.started = True

    async def stop(self) -> None:
        self.started = False


class DummyEurekaServer:
    def __init__(
        self,
        bundle: CertificateBundle,
        identity: object,
        *,
        host: str,
        https_port: int,
        http_port: int,
    ) -> None:
        _ = bundle
        _ = identity
        self.host = host
        self.https_port = https_port
        self.http_port = http_port
        self.started = False

    async def start(self, certificates: CertificateBundle) -> None:
        _ = certificates
        self.started = True

    async def stop(self) -> None:
        self.started = False


class DummyApp(AppProvider):
    def __init__(self) -> None:
        self.launch_calls: list[LaunchCredentials] = []
        self.custom_messages: list[tuple[str, dict[str, Any]]] = []
        self.configure_calls: list[dict[str, Any]] = []
        self.stop_calls = 0

    @override
    def app_ids(self) -> frozenset[str]:
        return frozenset({"DUMMYAPP"})

    @override
    def display_name(self) -> str:
        return "Dummy"

    @override
    def namespaces(self) -> frozenset[str]:
        return frozenset({"urn:x-cast:com.example.dummy"})

    @override
    def app_key(self) -> str:
        return "dummy"

    @override
    def configure(self, config: dict[str, Any]) -> None:
        self.configure_calls.append(config)

    @override
    async def on_launch(
        self,
        session: AppContext,
        credentials: LaunchCredentials,
    ) -> None:
        _ = session
        self.launch_calls.append(credentials)

    @override
    async def on_message(
        self,
        session: AppContext,
        namespace: str,
        data: dict[str, Any],
    ) -> AppMessageDisposition:
        _ = session
        self.custom_messages.append((namespace, data))
        return AppMessageDisposition.HANDLED

    @override
    async def resolve_media(
        self,
        session: AppContext,
        load_request: LoadRequest,
    ) -> PlaybackMedia:
        _ = load_request
        return PlaybackMedia(
            session_id=session.session_id,
            streams=(
                PlaybackStream(
                    url="https://example.com/video.mpd",
                    content_type="application/dash+xml",
                ),
            ),
            stream_type=StreamType.BUFFERED,
        )

    @override
    async def on_stop(self, session: AppContext) -> None:
        _ = session
        self.stop_calls += 1


def _patch_runtime(monkeypatch: Any, *, crl: bytes = b"test-crl") -> None:
    monkeypatch.setattr("vibecast.receiver.CastAdvertisement", DummyAdvertisement)
    monkeypatch.setattr("vibecast.receiver.EurekaServer", DummyEurekaServer)
    monkeypatch.setattr("vibecast.receiver._CAST_PORT", 0)

    async def fake_fetch_crl(*, client: Any | None = None) -> bytes:
        _ = client
        return crl

    monkeypatch.setattr("vibecast.receiver.fetch_crl", fake_fetch_crl)


def _receiver_config(
    *,
    friendly_name: str = "Living Room",
    bind_host: str = "0.0.0.0",
    player_port: int = 8010,
) -> VibecastConfig:
    return VibecastConfig(
        device=replace(DeviceConfig(), friendly_name=friendly_name),
        network=replace(
            NetworkConfig(),
            bind_host=bind_host,
            player_port=player_port,
        ),
    )


class TestReceiverConfig:
    def test_defaults(self, bundle: CertificateBundle) -> None:
        config = _receiver_config()
        receiver = CastReceiver(config=config, certificates=bundle, apps=[])

        assert config.device.model == "Chromecast"
        assert config.network.bind_host == "0.0.0.0"
        assert config.network.player_port == 8010
        assert receiver.device.config.device_id is not None


class TestConstruction:
    def test_explicit_providers_skip_discovery(
        self,
        monkeypatch: Any,
        bundle: CertificateBundle,
    ) -> None:
        def fail_discovery() -> list[AppProvider]:
            msg = "discover_apps should not run"
            raise AssertionError(msg)

        monkeypatch.setattr("vibecast.receiver.discover_apps", fail_discovery)
        app = DummyApp()

        receiver = CastReceiver(
            config=_receiver_config(),
            certificates=bundle,
            apps=[app],
        )

        assert receiver.apps.get("DUMMYAPP") is app
        assert app.configure_calls == [{}]

    def test_app_receives_config_section(self, bundle: CertificateBundle) -> None:
        app = DummyApp()
        receiver = CastReceiver(
            config=replace(
                _receiver_config(),
                apps={"dummy": {"country": "se"}},
            ),
            certificates=bundle,
            apps=[app],
        )

        assert receiver.apps.get("DUMMYAPP") is app
        assert app.configure_calls == [{"country": "se"}]


class TestIntegration:
    async def test_start_auth_and_get_status(
        self,
        monkeypatch: Any,
        bundle: CertificateBundle,
        ssl_client_context: SSLContext,
    ) -> None:
        _patch_runtime(monkeypatch, crl=b"\xaa\xbb")
        receiver = CastReceiver(
            config=_receiver_config(bind_host="127.0.0.1", player_port=0),
            certificates=bundle,
            apps=[],
        )
        await receiver.start()
        port = receiver.serving_port
        assert port is not None

        reader, writer = await asyncio.open_connection(
            "127.0.0.1",
            port,
            ssl=ssl_client_context,
        )

        auth = DeviceAuthMessage(challenge=AuthChallenge())
        writer.write(
            _frame(
                make_cast_message(
                    source="sender-0",
                    destination="receiver-0",
                    namespace=ns.DEVICE_AUTH,
                    payload_binary=auth.SerializeToString(),
                )
            )
        )
        await writer.drain()

        auth_resp = await _read_framed(reader)
        decoded = DeviceAuthMessage()
        _ = decoded.ParseFromString(auth_resp.payload_binary)
        assert decoded.response.crl == b"\xaa\xbb"

        writer.write(
            _frame(
                make_cast_message(
                    source="sender-0",
                    destination="receiver-0",
                    namespace=ns.RECEIVER,
                    payload_utf8='{"type":"GET_STATUS","requestId":1}',
                )
            )
        )
        await writer.drain()

        status_resp = await _read_framed(reader)
        payload = json.loads(status_resp.payload_utf8)
        assert payload["type"] == "RECEIVER_STATUS"
        assert payload["requestId"] == 1

        writer.close()
        await writer.wait_closed()
        await receiver.stop()

    async def test_run_forever_can_be_cancelled(
        self,
        monkeypatch: Any,
        bundle: CertificateBundle,
    ) -> None:
        _patch_runtime(monkeypatch)
        receiver = CastReceiver(
            config=_receiver_config(bind_host="127.0.0.1", player_port=0),
            certificates=bundle,
            apps=[],
        )

        task = asyncio.create_task(receiver.run_forever())
        for _ in range(50):
            if receiver.serving_port is not None:
                break
            await asyncio.sleep(0.01)

        _ = task.cancel()
        with pytest.raises(asyncio.CancelledError):
            await task

        assert receiver.serving_port is None

    async def test_launch_visible_in_receiver_status(
        self,
        monkeypatch: Any,
        bundle: CertificateBundle,
        ssl_client_context: SSLContext,
    ) -> None:
        _patch_runtime(monkeypatch)
        app = DummyApp()
        receiver = CastReceiver(
            config=_receiver_config(bind_host="127.0.0.1", player_port=0),
            certificates=bundle,
            apps=[app],
        )
        await receiver.start()
        port = receiver.serving_port
        assert port is not None

        reader, writer = await asyncio.open_connection(
            "127.0.0.1",
            port,
            ssl=ssl_client_context,
        )

        writer.write(
            _frame(
                make_cast_message(
                    namespace=ns.CONNECTION,
                    payload_utf8='{"type":"CONNECT","origin":{}}',
                )
            )
        )
        await writer.drain()

        writer.write(
            _frame(
                make_cast_message(
                    namespace=ns.RECEIVER,
                    payload_utf8='{"type":"LAUNCH","requestId":2,"appId":"DUMMYAPP","credentials":"token","credentialsType":"bearer"}',
                )
            )
        )
        await writer.drain()

        launch_status = await _read_framed(reader)
        payload = json.loads(launch_status.payload_utf8)
        assert payload["type"] == "RECEIVER_STATUS"
        applications = payload["status"]["applications"]
        assert len(applications) == 1
        assert applications[0]["appId"] == "DUMMYAPP"
        transport_id = applications[0]["transportId"]
        assert app.launch_calls[0].credentials == "token"
        assert app.launch_calls[0].credentials_type == "bearer"

        writer.write(
            _frame(
                make_cast_message(
                    destination=transport_id,
                    namespace=ns.CONNECTION,
                    payload_utf8='{"type":"CONNECT","origin":{}}',
                )
            )
        )
        await writer.drain()

        writer.write(
            _frame(
                make_cast_message(
                    destination=transport_id,
                    namespace="urn:x-cast:com.example.dummy",
                    payload_utf8='{"type":"HELLO"}',
                )
            )
        )
        await writer.drain()

        for _ in range(50):
            if app.custom_messages:
                break
            await asyncio.sleep(0.01)

        assert app.custom_messages == [
            ("urn:x-cast:com.example.dummy", {"type": "HELLO"})
        ]

        writer.close()
        await writer.wait_closed()

        for _ in range(50):
            if receiver.device.session_ids():
                break
            await asyncio.sleep(0.01)

        assert app.stop_calls == 0
        assert len(receiver.device.session_ids()) == 1

        await receiver.stop()
        assert app.stop_calls == 1


class TestStartupLogging:
    async def test_start_logs_enabled_providers(
        self,
        monkeypatch: Any,
        bundle: CertificateBundle,
        caplog: pytest.LogCaptureFixture,
    ) -> None:
        _patch_runtime(monkeypatch)
        receiver = CastReceiver(
            config=_receiver_config(bind_host="127.0.0.1", player_port=0),
            certificates=bundle,
            apps=[DummyApp()],
        )
        caplog.set_level("INFO", logger="vibecast.receiver")

        await receiver.start()
        await receiver.stop()

        assert any(
            record.name == "vibecast.receiver"
            and record.getMessage() == "enabled apps: Dummy (appIds=DUMMYAPP)"
            for record in caplog.records
        )
