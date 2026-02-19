"""Length-prefixed protobuf message framing for the CastV2 protocol.

All Cast messages are framed as:
    ┌──────────────────┬─────────────────────────────┐
    │ 4 bytes (BE u32) │ N bytes (protobuf payload)   │
    │ payload length   │ serialized CastMessage       │
    └──────────────────┴─────────────────────────────┘
"""

import asyncio
import struct

from castvibe._proto.cast_channel_pb2 import CastMessage

#: Maximum allowed message size (64 KiB). Cast messages are typically a few KB;
#: this limit protects against malformed streams.
MAX_MESSAGE_SIZE = 64 * 1024

#: Struct format for the 4-byte big-endian unsigned length prefix.
_LENGTH_PREFIX = struct.Struct(">I")


class FramingError(Exception):
    """Raised for protocol-level framing errors."""


async def read_message(reader: asyncio.StreamReader) -> CastMessage:
    """Read a single length-prefixed CastMessage from *reader*.

    Raises:
        FramingError: On zero-length or oversized message.
        asyncio.IncompleteReadError: On connection close or incomplete read.
    """
    # Read 4-byte length prefix.
    header = await reader.readexactly(_LENGTH_PREFIX.size)
    (length,) = _LENGTH_PREFIX.unpack(header)

    if length == 0:
        msg = "Received zero-length message"
        raise FramingError(msg)

    if length > MAX_MESSAGE_SIZE:
        msg = f"Message too large: {length} bytes (max {MAX_MESSAGE_SIZE})"
        raise FramingError(msg)

    # Read the protobuf payload.
    payload = await reader.readexactly(length)

    cast_message = CastMessage()
    _ = cast_message.ParseFromString(payload)
    return cast_message


async def write_message(
    writer: asyncio.StreamWriter,
    msg: CastMessage,
) -> None:
    """Write a single length-prefixed CastMessage to *writer*.

    Raises:
        FramingError: If the serialized message exceeds *MAX_MESSAGE_SIZE*.
    """
    payload = msg.SerializeToString()
    if len(payload) > MAX_MESSAGE_SIZE:
        err = (
            f"Message too large to send: {len(payload)} bytes (max {MAX_MESSAGE_SIZE})"
        )
        raise FramingError(err)
    writer.write(_LENGTH_PREFIX.pack(len(payload)))
    writer.write(payload)
    await writer.drain()
