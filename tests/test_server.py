"""Tests for the asyncio TLS Cast server."""

from __future__ import annotations

import asyncio
import contextlib
import json
import ssl
import struct
from typing import TYPE_CHECKING

from tests.conftest import make_cast_message
from vibecast import _namespace as ns
from vibecast._proto.cast_channel_pb2 import (
    AuthChallenge,
    CastMessage,
    DeviceAuthMessage,
)
from vibecast._server import CastServer

if TYPE_CHECKING:
    from vibecast._certificate import CertificateBundle
    from vibecast._connection import Connection


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _frame(msg: CastMessage) -> bytes:
    """Serialize a CastMessage into wire format."""
    payload = msg.SerializeToString()
    return struct.pack(">I", len(payload)) + payload


async def _read_framed(reader: asyncio.StreamReader) -> CastMessage:
    """Read one length-prefixed CastMessage from a stream."""
    header = await asyncio.wait_for(reader.readexactly(4), timeout=5)
    (length,) = struct.unpack(">I", header)
    payload = await asyncio.wait_for(reader.readexactly(length), timeout=5)
    msg = CastMessage()
    _ = msg.ParseFromString(payload)
    return msg


def _get_port(server: CastServer) -> int:
    """Extract the listening port from a started server."""
    port = server.serving_port
    assert port is not None
    return port


# ---------------------------------------------------------------------------
# TLS handshake
# ---------------------------------------------------------------------------


class TestTLSHandshake:
    """Tests that the server accepts TLS connections."""

    async def test_tls_handshake_completes(
        self,
        bundle: CertificateBundle,
        ssl_client_context: ssl.SSLContext,
    ) -> None:
        """A TLS client can connect and complete the handshake."""
        server = CastServer(bundle, host="127.0.0.1", port=0)
        await server.start()
        port = _get_port(server)

        _reader, writer = await asyncio.open_connection(
            "127.0.0.1", port, ssl=ssl_client_context
        )
        # If we get here, the handshake succeeded.
        writer.close()
        await writer.wait_closed()
        await server.stop()

    async def test_multiple_connections(
        self,
        bundle: CertificateBundle,
        ssl_client_context: ssl.SSLContext,
    ) -> None:
        """Multiple clients can connect concurrently."""
        server = CastServer(bundle, host="127.0.0.1", port=0)
        await server.start()
        port = _get_port(server)

        writers: list[asyncio.StreamWriter] = []
        for _ in range(3):
            _reader, writer = await asyncio.open_connection(
                "127.0.0.1", port, ssl=ssl_client_context
            )
            writers.append(writer)

        for w in writers:
            w.close()
            await w.wait_closed()
        await server.stop()


# ---------------------------------------------------------------------------
# Device auth over TLS
# ---------------------------------------------------------------------------


class TestDeviceAuthOverTLS:
    """End-to-end device auth through the TLS server."""

    async def test_auth_challenge_response(
        self,
        bundle: CertificateBundle,
        ssl_client_context: ssl.SSLContext,
    ) -> None:
        """Sending a device auth challenge returns a valid auth response."""
        server = CastServer(bundle, host="127.0.0.1", port=0)
        await server.start()
        port = _get_port(server)

        reader, writer = await asyncio.open_connection(
            "127.0.0.1", port, ssl=ssl_client_context
        )

        # Send auth challenge.
        challenge = DeviceAuthMessage(challenge=AuthChallenge())
        msg = make_cast_message(
            source="sender-0",
            destination="receiver-0",
            namespace=ns.DEVICE_AUTH,
            payload_binary=challenge.SerializeToString(),
        )
        writer.write(_frame(msg))
        await writer.drain()

        # Read auth response.
        resp = await _read_framed(reader)
        assert resp.namespace == ns.DEVICE_AUTH
        assert resp.payload_type == CastMessage.BINARY

        auth_msg = DeviceAuthMessage()
        _ = auth_msg.ParseFromString(resp.payload_binary)
        assert auth_msg.HasField("response")
        assert auth_msg.response.signature == bundle.signature_sha1
        assert auth_msg.response.client_auth_certificate == bundle.device_cert_der
        assert len(auth_msg.response.intermediate_certificate) == 1

        writer.close()
        await writer.wait_closed()
        await server.stop()


# ---------------------------------------------------------------------------
# Heartbeat over TLS
# ---------------------------------------------------------------------------


class TestHeartbeatOverTLS:
    """End-to-end heartbeat through the TLS server."""

    async def test_ping_pong(
        self,
        bundle: CertificateBundle,
        ssl_client_context: ssl.SSLContext,
    ) -> None:
        """Sending PING returns PONG."""
        server = CastServer(bundle, host="127.0.0.1", port=0)
        await server.start()
        port = _get_port(server)

        reader, writer = await asyncio.open_connection(
            "127.0.0.1", port, ssl=ssl_client_context
        )

        ping = make_cast_message(
            source="sender-0",
            destination="receiver-0",
            namespace=ns.HEARTBEAT,
            payload_utf8='{"type":"PING"}',
        )
        writer.write(_frame(ping))
        await writer.drain()

        resp = await _read_framed(reader)
        assert resp.namespace == ns.HEARTBEAT
        parsed = json.loads(resp.payload_utf8)
        assert parsed["type"] == "PONG"
        assert resp.source_id == "receiver-0"
        assert resp.destination_id == "sender-0"

        writer.close()
        await writer.wait_closed()
        await server.stop()


# ---------------------------------------------------------------------------
# Server lifecycle
# ---------------------------------------------------------------------------


class TestServerLifecycle:
    """Tests for server start/stop behaviour."""

    async def test_stop_is_clean(self, bundle: CertificateBundle) -> None:
        """stop() completes without leaked tasks."""
        server = CastServer(bundle, host="127.0.0.1", port=0)
        await server.start()
        await server.stop()

        assert not server.is_serving
        assert server.active_connections == 0

    async def test_stop_without_start(self, bundle: CertificateBundle) -> None:
        """stop() on a never-started server is a no-op."""
        server = CastServer(bundle, host="127.0.0.1", port=0)
        await server.stop()  # should not raise

    async def test_stop_closes_active_connections(
        self,
        bundle: CertificateBundle,
        ssl_client_context: ssl.SSLContext,
    ) -> None:
        """stop() cancels connection handler tasks and cleans up."""
        connected = asyncio.Event()

        async def on_conn(_conn: Connection) -> None:
            connected.set()

        server = CastServer(bundle, host="127.0.0.1", port=0, on_connection=on_conn)
        await server.start()
        port = _get_port(server)

        _reader, writer = await asyncio.open_connection(
            "127.0.0.1", port, ssl=ssl_client_context
        )

        _ = await asyncio.wait_for(connected.wait(), timeout=5)

        # The server should have at least one active connection.
        assert server.active_connections >= 1

        await server.stop()

        assert server.active_connections == 0

        writer.close()
        with contextlib.suppress(OSError, ssl.SSLError):
            await writer.wait_closed()


# ---------------------------------------------------------------------------
# Callback integration
# ---------------------------------------------------------------------------


class TestCallbacks:
    """Tests for on_connection and on_message callbacks."""

    async def test_on_connection_called(
        self,
        bundle: CertificateBundle,
        ssl_client_context: ssl.SSLContext,
    ) -> None:
        """on_connection is invoked when a new sender connects."""
        connections: list[Connection] = []
        connected = asyncio.Event()

        async def on_conn(conn: Connection) -> None:
            connections.append(conn)
            connected.set()

        server = CastServer(bundle, host="127.0.0.1", port=0, on_connection=on_conn)
        await server.start()
        port = _get_port(server)

        _reader, writer = await asyncio.open_connection(
            "127.0.0.1", port, ssl=ssl_client_context
        )

        _ = await asyncio.wait_for(connected.wait(), timeout=5)
        assert len(connections) == 1

        writer.close()
        await writer.wait_closed()
        await server.stop()

    async def test_on_message_receives_forwarded(
        self,
        bundle: CertificateBundle,
        ssl_client_context: ssl.SSLContext,
    ) -> None:
        """on_message receives non-platform messages sent by the client."""
        messages: list[CastMessage] = []
        received = asyncio.Event()

        async def on_msg(_conn: Connection, msg: CastMessage) -> None:
            messages.append(msg)
            received.set()

        server = CastServer(bundle, host="127.0.0.1", port=0, on_message=on_msg)
        await server.start()
        port = _get_port(server)

        _reader, writer = await asyncio.open_connection(
            "127.0.0.1", port, ssl=ssl_client_context
        )

        get_status = make_cast_message(
            source="sender-0",
            destination="receiver-0",
            namespace=ns.RECEIVER,
            payload_utf8='{"type":"GET_STATUS","requestId":1}',
        )
        writer.write(_frame(get_status))
        await writer.drain()

        _ = await asyncio.wait_for(received.wait(), timeout=5)
        assert len(messages) == 1
        assert messages[0].namespace == ns.RECEIVER

        writer.close()
        await writer.wait_closed()
        await server.stop()

    async def test_on_disconnect_called(
        self,
        bundle: CertificateBundle,
        ssl_client_context: ssl.SSLContext,
    ) -> None:
        """on_disconnect is invoked when a sender disconnects."""
        disconnected = asyncio.Event()

        async def on_disc(_conn: Connection) -> None:
            disconnected.set()

        server = CastServer(bundle, host="127.0.0.1", port=0, on_disconnect=on_disc)
        await server.start()
        port = _get_port(server)

        _reader, writer = await asyncio.open_connection(
            "127.0.0.1", port, ssl=ssl_client_context
        )
        # Allow the connection to be fully established.
        await asyncio.sleep(0.05)

        writer.close()
        await writer.wait_closed()

        _ = await asyncio.wait_for(disconnected.wait(), timeout=5)

        await server.stop()
