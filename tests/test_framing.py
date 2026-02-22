"""Tests for CastV2 length-prefixed message framing."""

import asyncio
import struct
from typing import override

import pytest

from vibecast._framing import (
    MAX_MESSAGE_SIZE,
    FramingError,
    read_message,
    write_message,
)
from vibecast._proto.cast_channel_pb2 import CastMessage


def _make_message(
    *,
    source: str = "sender-0",
    destination: str = "receiver-0",
    namespace: str = "urn:x-cast:com.google.cast.tp.heartbeat",
    payload_utf8: str | None = None,
    payload_binary: bytes | None = None,
) -> CastMessage:
    """Build a CastMessage for testing."""
    msg = CastMessage()
    msg.protocol_version = CastMessage.CASTV2_1_0
    msg.source_id = source
    msg.destination_id = destination
    msg.namespace = namespace
    if payload_binary is not None:
        msg.payload_type = CastMessage.BINARY
        msg.payload_binary = payload_binary
    else:
        msg.payload_type = CastMessage.STRING
        msg.payload_utf8 = payload_utf8 or ""
    return msg


def _frame(msg: CastMessage) -> bytes:
    """Serialize a CastMessage into its wire format (length prefix + protobuf)."""
    payload = msg.SerializeToString()
    return struct.pack(">I", len(payload)) + payload


class _BufferTransport(asyncio.Transport):
    """In-memory transport that captures all written bytes."""

    def __init__(self) -> None:
        super().__init__()
        self.buffer = bytearray()
        self._closing = False

    @override
    def write(self, data: bytes | bytearray | memoryview) -> None:
        self.buffer.extend(data)

    @override
    def is_closing(self) -> bool:
        return self._closing

    @override
    def close(self) -> None:
        self._closing = True


async def _roundtrip(msg: CastMessage) -> CastMessage:
    """Frame *msg* into a StreamReader and read it back via ``read_message``."""
    reader = asyncio.StreamReader()
    reader.feed_data(_frame(msg))
    return await read_message(reader)


async def _write_and_capture(msg: CastMessage) -> bytes:
    """Write *msg* via ``write_message`` and return the raw bytes produced."""
    transport = _BufferTransport()
    reader = asyncio.StreamReader()
    protocol = asyncio.StreamReaderProtocol(reader)
    writer = asyncio.StreamWriter(
        transport, protocol, reader, asyncio.get_running_loop()
    )
    await write_message(writer, msg)
    return bytes(transport.buffer)


async def test_roundtrip_string_payload() -> None:
    """Round-trip a CastMessage with a STRING (JSON) payload."""
    original = _make_message(
        payload_utf8='{"type": "PING"}',
        namespace="urn:x-cast:com.google.cast.tp.heartbeat",
    )
    result = await _roundtrip(original)

    assert result.protocol_version == CastMessage.CASTV2_1_0
    assert result.source_id == "sender-0"
    assert result.destination_id == "receiver-0"
    assert result.namespace == "urn:x-cast:com.google.cast.tp.heartbeat"
    assert result.payload_type == CastMessage.STRING
    assert result.payload_utf8 == '{"type": "PING"}'


async def test_roundtrip_binary_payload() -> None:
    """Round-trip a CastMessage with a BINARY payload."""
    binary_data = b"\x00\x01\x02\xff" * 16
    original = _make_message(
        namespace="urn:x-cast:com.google.cast.tp.deviceauth",
        payload_binary=binary_data,
    )
    result = await _roundtrip(original)

    assert result.payload_type == CastMessage.BINARY
    assert result.payload_binary == binary_data


async def test_roundtrip_preserves_all_fields() -> None:
    """Ensure all CastMessage fields survive the round-trip."""
    original = _make_message(
        source="sender-42",
        destination="pid-7",
        namespace="urn:x-cast:com.google.cast.media",
        payload_utf8='{"type": "LOAD", "requestId": 1}',
    )
    result = await _roundtrip(original)

    assert result.source_id == original.source_id
    assert result.destination_id == original.destination_id
    assert result.namespace == original.namespace
    assert result.payload_utf8 == original.payload_utf8


async def test_write_message_produces_correct_frame() -> None:
    """write_message outputs a valid length-prefixed protobuf frame."""
    msg = _make_message(payload_utf8='{"type": "PONG"}')
    raw = await _write_and_capture(msg)

    # The output should match our manual framing.
    assert raw == _frame(msg)

    # Additionally, we can read it back to prove correctness.
    reader = asyncio.StreamReader()
    reader.feed_data(raw)
    result = await read_message(reader)
    assert result.payload_utf8 == '{"type": "PONG"}'


async def test_read_from_closed_stream() -> None:
    """Reading from an empty/closed stream raises an error."""
    reader = asyncio.StreamReader()
    reader.feed_eof()

    with pytest.raises(asyncio.IncompleteReadError):
        _ = await read_message(reader)


async def test_read_incomplete_header() -> None:
    """Reading with an incomplete header raises an error."""
    reader = asyncio.StreamReader()
    reader.feed_data(b"\x00\x00")  # only 2 of 4 header bytes
    reader.feed_eof()

    with pytest.raises(asyncio.IncompleteReadError):
        _ = await read_message(reader)


async def test_read_incomplete_payload() -> None:
    """Reading with a truncated payload raises an error."""
    reader = asyncio.StreamReader()
    # Header says 100 bytes, but we only supply 10.
    reader.feed_data(struct.pack(">I", 100) + b"\x00" * 10)
    reader.feed_eof()

    with pytest.raises(asyncio.IncompleteReadError):
        _ = await read_message(reader)


async def test_oversized_message_rejected() -> None:
    """Messages exceeding MAX_MESSAGE_SIZE are rejected."""
    reader = asyncio.StreamReader()
    oversized_length = MAX_MESSAGE_SIZE + 1
    reader.feed_data(struct.pack(">I", oversized_length))

    with pytest.raises(FramingError, match="Message too large"):
        _ = await read_message(reader)


async def test_zero_length_message_rejected() -> None:
    """Zero-length messages are rejected."""
    reader = asyncio.StreamReader()
    reader.feed_data(struct.pack(">I", 0))

    with pytest.raises(FramingError, match="zero-length"):
        _ = await read_message(reader)


async def test_write_oversized_message_rejected() -> None:
    """write_message rejects messages exceeding MAX_MESSAGE_SIZE."""
    msg = _make_message(payload_binary=b"\x00" * (MAX_MESSAGE_SIZE + 1))

    with pytest.raises(FramingError, match="too large to send"):
        _ = await _write_and_capture(msg)
