"""Per-client Cast connection handler.

Wraps an asyncio reader/writer pair and dispatches incoming Cast messages
by namespace.  Device authentication and heartbeat are handled locally;
all other messages are forwarded to an optional callback.
"""

from __future__ import annotations

import asyncio
import contextlib
import json
import logging
from typing import TYPE_CHECKING, Any

import castvibe._namespace as ns
from castvibe._auth import build_auth_response
from castvibe._framing import FramingError, read_message, write_message
from castvibe._log import get_logger
from castvibe._proto.cast_channel_pb2 import (
    CastMessage,
    DeviceAuthMessage,
    HashAlgorithm,
    SignatureAlgorithm,
)

if TYPE_CHECKING:
    from collections.abc import Awaitable, Callable

    from castvibe._certificate import CertificateBundle

log = get_logger("connection")


class Connection:
    """A single Cast sender connection.

    Reads length-prefixed protobuf frames, dispatches by namespace:

    * **deviceauth** — binary auth challenge → auth response (handled locally)
    * **heartbeat** — PING → PONG (handled locally)
    * **everything else** — forwarded to *on_message* callback

    Parameters
    ----------
    reader:
        The asyncio stream reader for this connection.
    writer:
        The asyncio stream writer for this connection.
    bundle:
        Certificate material for device authentication responses.
    on_message:
        Optional async callback ``(connection, cast_message) -> None``
        invoked for every message not handled locally.
    on_disconnect:
        Optional async callback ``(connection) -> None`` invoked when
        the connection is closed (cleanly or due to error).
    """

    __slots__ = (
        "_bundle",
        "_crl",
        "_on_disconnect",
        "_on_message",
        "_reader",
        "_writer",
        "peer",
    )

    def __init__(
        self,
        reader: asyncio.StreamReader,
        writer: asyncio.StreamWriter,
        bundle: CertificateBundle,
        *,
        crl: bytes | None = None,
        on_message: Callable[[Connection, CastMessage], Awaitable[None]] | None = None,
        on_disconnect: Callable[[Connection], Awaitable[None]] | None = None,
    ) -> None:
        self._reader = reader
        self._writer = writer
        self._bundle = bundle
        # CRL is captured at connection-accept time; if CRL rotation is
        # added later, connections should reference the server's CRL instead.
        self._crl = crl
        self._on_message = on_message
        self._on_disconnect = on_disconnect

        # Best-effort peer address for logging.
        peername = writer.get_extra_info("peername")
        self.peer: str = f"{peername[0]}:{peername[1]}" if peername else "unknown"

    # ------------------------------------------------------------------
    # Main loop
    # ------------------------------------------------------------------

    async def handle(self) -> None:
        """Run the message loop until the connection closes.

        This method reads messages in a loop and dispatches them by
        namespace.  It returns when the peer disconnects or an
        unrecoverable framing error is encountered.
        """
        log.info("connection opened: %s", self.peer)
        try:
            await self._loop()
        finally:
            self._writer.close()
            # writer.wait_closed() may raise if the transport is already
            # gone; swallow that so cleanup always completes.
            with contextlib.suppress(OSError):
                await self._writer.wait_closed()
            log.info("connection closed: %s", self.peer)
            if self._on_disconnect is not None:
                await self._on_disconnect(self)

    async def _loop(self) -> None:
        """Read messages until EOF or fatal error."""
        while True:
            try:
                msg = await read_message(self._reader)
            except asyncio.IncompleteReadError:
                # Clean disconnect (EOF).
                return
            except FramingError:
                log.warning("%s: framing error, closing", self.peer, exc_info=True)
                return
            except (ConnectionResetError, BrokenPipeError):
                # Transport-level disconnect.
                return

            try:
                await self._dispatch(msg)
            except Exception:
                # Per-message errors must not kill the connection.
                log.warning(
                    "%s: error handling message on %s",
                    self.peer,
                    msg.namespace,
                    exc_info=True,
                )

    # ------------------------------------------------------------------
    # Dispatch
    # ------------------------------------------------------------------

    async def _dispatch(self, msg: CastMessage) -> None:
        """Route *msg* to the correct handler by namespace."""
        # Warn on unexpected protocol version (only one has ever existed).
        if msg.protocol_version != CastMessage.CASTV2_1_0:
            log.warning(
                "%s: unexpected protocol version %s",
                self.peer,
                msg.protocol_version,
            )

        namespace = msg.namespace

        if namespace == ns.DEVICE_AUTH:
            await self._handle_device_auth(msg)
        elif namespace == ns.HEARTBEAT:
            await self._handle_heartbeat(msg)
        else:
            # Log non-heartbeat messages at DEBUG for diagnostics.
            _log_message(msg, self.peer)
            if self._on_message is not None:
                await self._on_message(self, msg)

    # ------------------------------------------------------------------
    # Local handlers
    # ------------------------------------------------------------------

    async def _handle_device_auth(self, msg: CastMessage) -> None:
        """Respond to a device-auth challenge with the pre-computed auth response."""
        challenge = DeviceAuthMessage()
        if not challenge.ParseFromString(msg.payload_binary):
            log.warning("%s: failed to parse device auth challenge", self.peer)
            return

        if challenge.HasField("challenge"):
            challenge_msg = challenge.challenge
            sender_nonce = challenge_msg.sender_nonce
            log.debug(
                "%s: device auth challenge received (hash=%s sig=%s nonce_len=%d)",
                self.peer,
                _enum_name(HashAlgorithm, challenge_msg.hash_algorithm),
                _enum_name(
                    SignatureAlgorithm,
                    challenge_msg.signature_algorithm,
                ),
                len(sender_nonce),
            )
        else:
            log.debug(
                "%s: device auth challenge received (no challenge field)", self.peer
            )

        payload = build_auth_response(self._bundle, crl=self._crl)
        await self.send_binary(
            source_id=msg.destination_id,
            dest_id=msg.source_id,
            namespace=ns.DEVICE_AUTH,
            data=payload,
        )
        response = DeviceAuthMessage()
        _ = response.ParseFromString(payload)
        if response.HasField("response"):
            response_msg = response.response
            log.debug(
                "%s: device auth response sent (hash=%s sig=%s ica=%d crl_len=%d nonce_len=%d)",
                self.peer,
                _enum_name(HashAlgorithm, response_msg.hash_algorithm),
                _enum_name(
                    SignatureAlgorithm,
                    response_msg.signature_algorithm,
                ),
                len(response_msg.intermediate_certificate),
                len(response_msg.crl),
                len(response_msg.sender_nonce),
            )
        else:
            log.debug("%s: device auth response sent", self.peer)

    async def _handle_heartbeat(self, msg: CastMessage) -> None:
        """Respond to PING with PONG (fast-path, no JSON parsing)."""
        if msg.payload_type == CastMessage.STRING and '"PING"' in msg.payload_utf8:
            await self.send_json(
                source_id=msg.destination_id,
                dest_id=msg.source_id,
                namespace=ns.HEARTBEAT,
                data={"type": "PONG"},
            )

    # ------------------------------------------------------------------
    # Sending helpers
    # ------------------------------------------------------------------

    async def send_json(
        self,
        source_id: str,
        dest_id: str,
        namespace: str,
        data: dict[str, Any],
    ) -> None:
        """Send a JSON (STRING) Cast message."""
        msg = _build_message(
            source_id=source_id,
            dest_id=dest_id,
            namespace=namespace,
            payload_type=CastMessage.STRING,
            payload_utf8=json.dumps(data, separators=(",", ":")),
        )
        await write_message(self._writer, msg)

    async def send_binary(
        self,
        source_id: str,
        dest_id: str,
        namespace: str,
        data: bytes,
    ) -> None:
        """Send a binary (BINARY) Cast message."""
        msg = _build_message(
            source_id=source_id,
            dest_id=dest_id,
            namespace=namespace,
            payload_type=CastMessage.BINARY,
            payload_binary=data,
        )
        await write_message(self._writer, msg)


# ------------------------------------------------------------------
# Helpers
# ------------------------------------------------------------------


def _build_message(
    *,
    source_id: str,
    dest_id: str,
    namespace: str,
    payload_type: CastMessage.PayloadType,
    payload_utf8: str = "",
    payload_binary: bytes = b"",
) -> CastMessage:
    """Construct a ``CastMessage`` with all required fields."""
    msg = CastMessage()
    msg.protocol_version = CastMessage.CASTV2_1_0
    msg.source_id = source_id
    msg.destination_id = dest_id
    msg.namespace = namespace
    msg.payload_type = payload_type
    if payload_type == CastMessage.BINARY:
        msg.payload_binary = payload_binary
    else:
        msg.payload_utf8 = payload_utf8
    return msg


def _log_message(msg: CastMessage, peer: str) -> None:
    """Emit a DEBUG log line summarising a Cast message."""
    if not log.isEnabledFor(logging.DEBUG):
        return
    if msg.payload_type == CastMessage.BINARY:
        payload_repr = f"<{len(msg.payload_binary)} bytes>"
    else:
        payload_repr = msg.payload_utf8
    log.debug(
        "%s: ns=%s src=%s dst=%s payload=%s",
        peer,
        msg.namespace,
        msg.source_id,
        msg.destination_id,
        payload_repr,
    )


def _enum_name(enum_type: type[object], value: int) -> str:
    """Best-effort enum name formatting for protobuf debug logs."""
    name_fn = getattr(enum_type, "Name", None)
    if callable(name_fn):
        try:
            return str(name_fn(value))
        except ValueError:
            return f"UNKNOWN({value})"
    return str(value)
