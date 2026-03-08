"""Tests for the per-client Cast connection handler."""

from __future__ import annotations

import asyncio
import json
import struct
from typing import TYPE_CHECKING, override
from unittest.mock import AsyncMock

from tests.conftest import frame_message, make_cast_message
from vibecast._proto.cast_channel_pb2 import (
    AuthChallenge,
    CastMessage,
    DeviceAuthMessage,
    HashAlgorithm,
)
from vibecast._transport import namespace as ns
from vibecast._transport.connection import Connection

if TYPE_CHECKING:
    from vibecast._security.certificate import CertificateBundle


# ---------------------------------------------------------------------------
# In-memory transport helpers
# ---------------------------------------------------------------------------


class _BufferTransport(asyncio.Transport):
    """In-memory transport that captures all written bytes.

    When :meth:`close` is called, ``connection_lost`` is scheduled on the
    protocol so that :meth:`StreamWriter.wait_closed` completes.
    """

    def __init__(self) -> None:
        super().__init__(extra={"peername": ("127.0.0.1", 9999)})
        self.buffer = bytearray()
        self._closing = False
        self.protocol_ref: asyncio.BaseProtocol | None = None

    @override
    def write(self, data: bytes | bytearray | memoryview) -> None:
        self.buffer.extend(data)

    @override
    def is_closing(self) -> bool:
        return self._closing

    @override
    def close(self) -> None:
        if not self._closing:
            self._closing = True
            if self.protocol_ref is not None:
                _ = asyncio.get_event_loop().call_soon(
                    self.protocol_ref.connection_lost, None
                )


def _make_writer() -> tuple[asyncio.StreamWriter, _BufferTransport]:
    """Create a StreamWriter backed by an in-memory transport."""
    transport = _BufferTransport()
    reader = asyncio.StreamReader()
    protocol = asyncio.StreamReaderProtocol(reader)
    # Wire up protocol ↔ transport so close/wait_closed work correctly.
    protocol.connection_made(transport)
    transport.protocol_ref = protocol
    writer = asyncio.StreamWriter(
        transport, protocol, reader, asyncio.get_running_loop()
    )
    return writer, transport


def _make_connection(
    bundle: CertificateBundle,
    *frames: bytes,
    on_message: AsyncMock | None = None,
    on_disconnect: AsyncMock | None = None,
) -> tuple[Connection, _BufferTransport]:
    """Build a Connection with pre-loaded input frames and an in-memory output."""
    reader = asyncio.StreamReader()
    for frame in frames:
        reader.feed_data(frame)
    reader.feed_eof()

    writer, transport = _make_writer()

    conn = Connection(
        reader,
        writer,
        bundle,
        on_message=on_message,
        on_disconnect=on_disconnect,
    )
    return conn, transport


def _read_response(buf: bytes | bytearray) -> CastMessage:
    """Parse the first framed CastMessage from raw bytes."""
    length = struct.unpack(">I", buf[:4])[0]
    msg = CastMessage()
    _ = msg.ParseFromString(bytes(buf[4 : 4 + length]))
    return msg


# ---------------------------------------------------------------------------
# send_json / send_binary
# ---------------------------------------------------------------------------


class TestSendJson:
    """Tests for Connection.send_json()."""

    async def test_produces_string_message(self, bundle: CertificateBundle) -> None:
        """send_json builds a STRING CastMessage with JSON payload."""
        conn, transport = _make_connection(bundle)

        await conn.send_json(
            source_id="receiver-0",
            dest_id="sender-0",
            namespace=ns.RECEIVER,
            data={"type": "RECEIVER_STATUS", "requestId": 1},
        )

        msg = _read_response(transport.buffer)
        assert msg.payload_type == CastMessage.STRING
        assert msg.protocol_version == CastMessage.CASTV2_1_0
        assert msg.source_id == "receiver-0"
        assert msg.destination_id == "sender-0"
        assert msg.namespace == ns.RECEIVER

        parsed = json.loads(msg.payload_utf8)
        assert parsed["type"] == "RECEIVER_STATUS"
        assert parsed["requestId"] == 1

    async def test_compact_json(self, bundle: CertificateBundle) -> None:
        """JSON output uses compact separators (no extra spaces)."""
        conn, transport = _make_connection(bundle)

        await conn.send_json(
            source_id="receiver-0",
            dest_id="sender-0",
            namespace=ns.HEARTBEAT,
            data={"type": "PONG"},
        )

        msg = _read_response(transport.buffer)
        assert " " not in msg.payload_utf8


class TestSendBinary:
    """Tests for Connection.send_binary()."""

    async def test_produces_binary_message(self, bundle: CertificateBundle) -> None:
        """send_binary builds a BINARY CastMessage with the provided payload."""
        conn, transport = _make_connection(bundle)
        payload = b"\x0a\x0b\x0c"

        await conn.send_binary(
            source_id="receiver-0",
            dest_id="sender-0",
            namespace=ns.DEVICE_AUTH,
            data=payload,
        )

        msg = _read_response(transport.buffer)
        assert msg.payload_type == CastMessage.BINARY
        assert msg.protocol_version == CastMessage.CASTV2_1_0
        assert msg.payload_binary == payload
        assert msg.source_id == "receiver-0"
        assert msg.destination_id == "sender-0"


# ---------------------------------------------------------------------------
# Heartbeat dispatch
# ---------------------------------------------------------------------------


class TestHeartbeat:
    """Tests for PING → PONG heartbeat handling."""

    async def test_ping_gets_pong(self, bundle: CertificateBundle) -> None:
        """A PING on the heartbeat namespace produces a PONG response."""
        ping = make_cast_message(
            source="sender-0",
            destination="receiver-0",
            namespace=ns.HEARTBEAT,
            payload_utf8='{"type":"PING"}',
        )
        conn, transport = _make_connection(bundle, frame_message(ping))
        await conn.handle()

        msg = _read_response(transport.buffer)
        assert msg.namespace == ns.HEARTBEAT
        parsed = json.loads(msg.payload_utf8)
        assert parsed["type"] == "PONG"

    async def test_source_dest_swapped(self, bundle: CertificateBundle) -> None:
        """PONG response swaps source and destination IDs."""
        ping = make_cast_message(
            source="sender-42",
            destination="receiver-0",
            namespace=ns.HEARTBEAT,
            payload_utf8='{"type":"PING"}',
        )
        conn, transport = _make_connection(bundle, frame_message(ping))
        await conn.handle()

        msg = _read_response(transport.buffer)
        assert msg.source_id == "receiver-0"
        assert msg.destination_id == "sender-42"

    async def test_non_ping_ignored(self, bundle: CertificateBundle) -> None:
        """A PONG on the heartbeat namespace does not produce a response."""
        pong = make_cast_message(
            namespace=ns.HEARTBEAT,
            payload_utf8='{"type":"PONG"}',
        )
        conn, transport = _make_connection(bundle, frame_message(pong))
        await conn.handle()

        assert len(transport.buffer) == 0

    async def test_heartbeat_not_forwarded(self, bundle: CertificateBundle) -> None:
        """Heartbeat messages are never forwarded to on_message."""
        on_msg = AsyncMock()
        ping = make_cast_message(
            namespace=ns.HEARTBEAT,
            payload_utf8='{"type":"PING"}',
        )
        conn, _transport = _make_connection(
            bundle, frame_message(ping), on_message=on_msg
        )
        await conn.handle()

        on_msg.assert_not_called()


# ---------------------------------------------------------------------------
# Device auth dispatch
# ---------------------------------------------------------------------------


class TestDeviceAuth:
    """Tests for device authentication challenge handling."""

    async def test_auth_response_returned(self, bundle: CertificateBundle) -> None:
        """A device-auth challenge gets a valid DeviceAuthMessage response."""
        challenge_msg = DeviceAuthMessage(challenge=AuthChallenge())
        challenge = make_cast_message(
            source="sender-0",
            destination="receiver-0",
            namespace=ns.DEVICE_AUTH,
            payload_binary=challenge_msg.SerializeToString(),
        )
        conn, transport = _make_connection(bundle, frame_message(challenge))
        await conn.handle()

        msg = _read_response(transport.buffer)
        assert msg.payload_type == CastMessage.BINARY
        assert msg.namespace == ns.DEVICE_AUTH

        auth_msg = DeviceAuthMessage()
        _ = auth_msg.ParseFromString(msg.payload_binary)
        assert auth_msg.HasField("response")
        assert auth_msg.response.signature == bundle.signature_sha1
        assert auth_msg.response.client_auth_certificate == bundle.device_cert_der

    async def test_auth_response_uses_requested_sha256(
        self, bundle: CertificateBundle
    ) -> None:
        challenge_msg = DeviceAuthMessage(
            challenge=AuthChallenge(hash_algorithm=HashAlgorithm.SHA256)
        )
        challenge = make_cast_message(
            source="sender-0",
            destination="receiver-0",
            namespace=ns.DEVICE_AUTH,
            payload_binary=challenge_msg.SerializeToString(),
        )
        conn, transport = _make_connection(bundle, frame_message(challenge))
        await conn.handle()

        msg = _read_response(transport.buffer)
        auth_msg = DeviceAuthMessage()
        _ = auth_msg.ParseFromString(msg.payload_binary)

        assert auth_msg.HasField("response")
        assert auth_msg.response.hash_algorithm == HashAlgorithm.SHA256
        assert auth_msg.response.signature == bundle.signature_sha256

    async def test_auth_source_dest_swapped(self, bundle: CertificateBundle) -> None:
        """Auth response swaps source and destination IDs."""
        challenge_msg = DeviceAuthMessage(challenge=AuthChallenge())
        challenge = make_cast_message(
            source="sender-0",
            destination="receiver-0",
            namespace=ns.DEVICE_AUTH,
            payload_binary=challenge_msg.SerializeToString(),
        )
        conn, transport = _make_connection(bundle, frame_message(challenge))
        await conn.handle()

        msg = _read_response(transport.buffer)
        assert msg.source_id == "receiver-0"
        assert msg.destination_id == "sender-0"

    async def test_auth_not_forwarded(self, bundle: CertificateBundle) -> None:
        """Device-auth messages are never forwarded to on_message."""
        on_msg = AsyncMock()
        challenge_msg = DeviceAuthMessage(challenge=AuthChallenge())
        challenge = make_cast_message(
            source="sender-0",
            destination="receiver-0",
            namespace=ns.DEVICE_AUTH,
            payload_binary=challenge_msg.SerializeToString(),
        )
        conn, _transport = _make_connection(
            bundle, frame_message(challenge), on_message=on_msg
        )
        await conn.handle()

        on_msg.assert_not_called()


# ---------------------------------------------------------------------------
# Message forwarding
# ---------------------------------------------------------------------------


class TestForwarding:
    """Tests for non-platform message forwarding."""

    async def test_receiver_message_forwarded(self, bundle: CertificateBundle) -> None:
        """Messages on the receiver namespace are forwarded to on_message."""
        on_msg = AsyncMock()
        get_status = make_cast_message(
            namespace=ns.RECEIVER,
            payload_utf8='{"type":"GET_STATUS","requestId":1}',
        )
        conn, _transport = _make_connection(
            bundle, frame_message(get_status), on_message=on_msg
        )
        await conn.handle()

        on_msg.assert_called_once()
        call_conn, call_msg = on_msg.call_args[0]
        assert call_conn is conn
        assert call_msg.namespace == ns.RECEIVER

    async def test_connection_message_forwarded(
        self, bundle: CertificateBundle
    ) -> None:
        """CONNECT messages on the connection namespace are forwarded."""
        on_msg = AsyncMock()
        connect = make_cast_message(
            namespace=ns.CONNECTION,
            payload_utf8='{"type":"CONNECT","origin":{}}',
        )
        conn, _transport = _make_connection(
            bundle, frame_message(connect), on_message=on_msg
        )
        await conn.handle()

        on_msg.assert_called_once()

    async def test_custom_namespace_forwarded(self, bundle: CertificateBundle) -> None:
        """Messages on custom (provider) namespaces are forwarded."""
        on_msg = AsyncMock()
        custom = make_cast_message(
            namespace="urn:x-cast:tv.viaplay.chromecast",
            payload_utf8='{"type":"SETUP_INFO"}',
        )
        conn, _transport = _make_connection(
            bundle, frame_message(custom), on_message=on_msg
        )
        await conn.handle()

        on_msg.assert_called_once()

    async def test_no_callback_does_not_crash(self, bundle: CertificateBundle) -> None:
        """Messages are silently dropped when no on_message callback is set."""
        get_status = make_cast_message(
            namespace=ns.RECEIVER,
            payload_utf8='{"type":"GET_STATUS","requestId":1}',
        )
        conn, _transport = _make_connection(bundle, frame_message(get_status))
        # Should complete without error.
        await conn.handle()


# ---------------------------------------------------------------------------
# Disconnect handling
# ---------------------------------------------------------------------------


class TestDisconnect:
    """Tests for connection lifecycle and disconnect."""

    async def test_clean_disconnect_calls_callback(
        self, bundle: CertificateBundle
    ) -> None:
        """on_disconnect is called when the peer disconnects cleanly."""
        on_disconnect = AsyncMock()
        conn, _transport = _make_connection(bundle, on_disconnect=on_disconnect)
        await conn.handle()

        on_disconnect.assert_called_once_with(conn)

    async def test_peer_address_set(self, bundle: CertificateBundle) -> None:
        """The peer attribute is populated from the writer's peername."""
        conn, _transport = _make_connection(bundle)
        assert conn.peer == "127.0.0.1:9999"


# ---------------------------------------------------------------------------
# Error resilience
# ---------------------------------------------------------------------------


class TestErrorResilience:
    """Tests for malformed message handling."""

    async def test_garbled_bytes_close_connection(
        self, bundle: CertificateBundle
    ) -> None:
        """Completely garbled data causes a clean exit (framing error)."""
        reader = asyncio.StreamReader()
        # Valid length prefix pointing to garbage protobuf.
        reader.feed_data(struct.pack(">I", 5) + b"\xff\xff\xff\xff\xff")
        reader.feed_eof()

        writer, _transport = _make_writer()
        on_disconnect = AsyncMock()

        conn = Connection(reader, writer, bundle, on_disconnect=on_disconnect)
        # Should not raise.
        await conn.handle()
        on_disconnect.assert_called_once()

    async def test_handler_error_does_not_kill_connection(
        self, bundle: CertificateBundle
    ) -> None:
        """An exception in on_message doesn't crash the connection loop."""

        async def failing_handler(_conn: Connection, _msg: CastMessage) -> None:
            msg = "boom"
            raise RuntimeError(msg)

        msg1 = make_cast_message(
            namespace=ns.RECEIVER,
            payload_utf8='{"type":"GET_STATUS","requestId":1}',
        )
        msg2 = make_cast_message(
            namespace=ns.RECEIVER,
            payload_utf8='{"type":"GET_STATUS","requestId":2}',
        )
        reader = asyncio.StreamReader()
        reader.feed_data(frame_message(msg1))
        reader.feed_data(frame_message(msg2))
        reader.feed_eof()

        writer, _transport = _make_writer()
        on_disconnect = AsyncMock()

        conn = Connection(
            reader,
            writer,
            bundle,
            on_message=failing_handler,
            on_disconnect=on_disconnect,
        )
        # Should process both messages despite the error and then exit cleanly.
        await conn.handle()
        on_disconnect.assert_called_once()

    async def test_multiple_messages_processed(self, bundle: CertificateBundle) -> None:
        """Multiple messages in sequence are all processed correctly."""
        on_msg = AsyncMock()
        ping = make_cast_message(
            namespace=ns.HEARTBEAT,
            payload_utf8='{"type":"PING"}',
        )
        get_status = make_cast_message(
            namespace=ns.RECEIVER,
            payload_utf8='{"type":"GET_STATUS","requestId":1}',
        )
        conn, transport = _make_connection(
            bundle,
            frame_message(ping),
            frame_message(get_status),
            on_message=on_msg,
        )
        await conn.handle()

        # PING should produce a PONG (in transport buffer).
        msg = _read_response(transport.buffer)
        assert json.loads(msg.payload_utf8)["type"] == "PONG"

        # GET_STATUS should be forwarded.
        on_msg.assert_called_once()
