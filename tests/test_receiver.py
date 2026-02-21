"""Tests for the public CastReceiver API."""

from __future__ import annotations

import asyncio
import json
import struct
from typing import TYPE_CHECKING, Any, override

import pytest

from castvibe import _namespace as ns
from castvibe._proto.cast_channel_pb2 import (
    AuthChallenge,
    CastMessage,
    DeviceAuthMessage,
)
from castvibe.provider import LaunchCredentials, Provider, ProviderSession
from castvibe.receiver import CastReceiver, ReceiverConfig
from tests.conftest import make_cast_message

if TYPE_CHECKING:
    from ssl import SSLContext

    from castvibe._certificate import CertificateBundle


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
    ) -> None:
        self.friendly_name = friendly_name
        self.device_model = device_model
        self.device_id = device_id
        self.port = port
        self.cert_digest = cert_digest
        self.started = False
        self.service_name = (
            f"castvibe-{device_id.replace('-', '')}._googlecast._tcp.local."
        )
        self.parsed_addresses = ("127.0.0.1",)

    async def start(self) -> None:
        self.started = True

    async def stop(self) -> None:
        self.started = False


class DummyProvider(Provider):
    def __init__(self) -> None:
        self.launch_calls: list[LaunchCredentials] = []
        self.custom_messages: list[tuple[str, dict[str, Any]]] = []
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
    async def on_launch(
        self,
        session: ProviderSession,
        credentials: LaunchCredentials,
    ) -> None:
        _ = session
        self.launch_calls.append(credentials)

    @override
    async def on_message(
        self,
        session: ProviderSession,
        namespace: str,
        data: dict[str, Any],
    ) -> None:
        _ = session
        self.custom_messages.append((namespace, data))

    @override
    async def on_stop(self, session: ProviderSession) -> None:
        _ = session
        self.stop_calls += 1


def _patch_runtime(monkeypatch: Any, *, crl: bytes = b"test-crl") -> None:
    monkeypatch.setattr("castvibe.receiver.CastAdvertisement", DummyAdvertisement)

    async def fake_fetch_crl(*, client: Any | None = None) -> bytes:
        _ = client
        return crl

    monkeypatch.setattr("castvibe.receiver.fetch_crl", fake_fetch_crl)


class TestReceiverConfig:
    def test_defaults(self, bundle: CertificateBundle) -> None:
        config = ReceiverConfig(friendly_name="Living Room")
        receiver = CastReceiver(config=config, certificates=bundle, providers=[])

        assert config.device_model == "Chromecast"
        assert config.host == "0.0.0.0"
        assert config.port == 8009
        assert receiver.config.device_id is not None


class TestConstruction:
    def test_explicit_providers_skip_discovery(
        self,
        monkeypatch: Any,
        bundle: CertificateBundle,
    ) -> None:
        def fail_discovery() -> list[Provider]:
            msg = "discover_providers should not run"
            raise AssertionError(msg)

        monkeypatch.setattr("castvibe.receiver.discover_providers", fail_discovery)
        provider = DummyProvider()

        receiver = CastReceiver(
            config=ReceiverConfig(friendly_name="Living Room"),
            certificates=bundle,
            providers=[provider],
        )

        assert receiver.providers.get("DUMMYAPP") is provider


class TestIntegration:
    async def test_start_auth_and_get_status(
        self,
        monkeypatch: Any,
        bundle: CertificateBundle,
        ssl_client_context: SSLContext,
    ) -> None:
        _patch_runtime(monkeypatch, crl=b"\xaa\xbb")
        receiver = CastReceiver(
            config=ReceiverConfig(
                friendly_name="Living Room", host="127.0.0.1", port=0
            ),
            certificates=bundle,
            providers=[],
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
            config=ReceiverConfig(
                friendly_name="Living Room", host="127.0.0.1", port=0
            ),
            certificates=bundle,
            providers=[],
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
        provider = DummyProvider()
        receiver = CastReceiver(
            config=ReceiverConfig(
                friendly_name="Living Room", host="127.0.0.1", port=0
            ),
            certificates=bundle,
            providers=[provider],
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
        assert provider.launch_calls[0].credentials == "token"
        assert provider.launch_calls[0].credentials_type == "bearer"

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
            if provider.custom_messages:
                break
            await asyncio.sleep(0.01)

        assert provider.custom_messages == [
            ("urn:x-cast:com.example.dummy", {"type": "HELLO"})
        ]

        writer.close()
        await writer.wait_closed()

        for _ in range(50):
            if receiver.device.session_ids():
                break
            await asyncio.sleep(0.01)

        assert provider.stop_calls == 0
        assert len(receiver.device.session_ids()) == 1

        await receiver.stop()
        assert provider.stop_calls == 1
