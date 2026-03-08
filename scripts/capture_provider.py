"""Cast protocol capture proxy for reverse-engineering new providers.

Sits between a Cast sender (e.g. iPhone) and a real Cast receiver (e.g.
NVIDIA Shield), forwarding all messages transparently while logging them
to a JSON Lines file.  Optionally launches ``mitmdump`` in WireGuard mode
to also capture the receiver's outbound HTTP/HTTPS traffic (API calls to
provider backends).

The resulting ``.jsonl`` file contains everything an LLM needs to
understand a provider's protocol and implement it as a vibecast provider.

Usage::

    # Cast-only capture:
    uv run python scripts/capture_provider.py \\
        --manifest /path/to/manifest.json \\
        --upstream 192.168.2.6

    # Full capture with HTTP interception (requires mitmproxy installed):
    uv run python scripts/capture_provider.py \\
        --manifest /path/to/manifest.json \\
        --upstream 192.168.2.6 \\
        --enable-mitm
"""

from __future__ import annotations

import argparse
import asyncio
import contextlib
import json
import os
import signal
import ssl
import subprocess
import sys
import tempfile
from datetime import UTC, datetime
from itertools import count
from pathlib import Path
from typing import Any
from uuid import uuid4

from vibecast._security.auth import build_auth_error, build_auth_response, fetch_crl
from vibecast._security.certificate import CertificateBundle, CertificateStore
from vibecast._discovery.mdns import CastAdvertisement
from vibecast._transport.framing import FramingError, read_message, write_message
from vibecast._transport.namespace import DEVICE_AUTH, HEARTBEAT, MEDIA, MULTIZONE, RECEIVER
from vibecast._proto.cast_channel_pb2 import (
    CastMessage,
    DeviceAuthMessage,
    HashAlgorithm,
    SignatureAlgorithm,
)
from vibecast._util import parse_json_payload

# ---------------------------------------------------------------------------
# Sequence counter shared across the process (not across OS processes).
# The mitmproxy addon uses its own independent counter — the consumer
# sorts primarily by timestamp.
# ---------------------------------------------------------------------------

_seq = count(1)
_DEFAULT_DEVICE_ID_PATH = Path.home() / ".vibecast" / "capture_proxy_device_id"


# ---------------------------------------------------------------------------
# Log writer
# ---------------------------------------------------------------------------


class LogWriter:
    """Append-mode JSON Lines writer.

    Each ``write()`` call emits exactly one ``\\n``-terminated JSON line.
    The file is opened in line-buffered mode so concurrent writes from the
    mitmproxy subprocess (via the same path) interleave safely at line
    boundaries.
    """

    def __init__(self, path: Path) -> None:
        self._path = path
        self._file = path.open("a", buffering=1)

    @property
    def path(self) -> Path:
        return self._path

    def write(self, entry: dict[str, Any]) -> None:
        line = json.dumps(entry, ensure_ascii=False, default=str)
        _ = self._file.write(line + "\n")
        self._file.flush()

    def close(self) -> None:
        self._file.close()

    # Convenience helpers ---------------------------------------------------

    def meta(self, event: str, **fields: Any) -> None:
        self.write(
            {
                "ts": _now(),
                "seq": next(_seq),
                "layer": "meta",
                "event": event,
                **fields,
            }
        )

    def cast(self, direction: str, msg: CastMessage, *, connection_id: int) -> None:
        payload: Any
        if msg.payload_type == CastMessage.STRING:
            payload = _try_parse_json(msg.payload_utf8)
        elif msg.payload_type == CastMessage.BINARY:
            payload = _decode_binary_payload(msg)
        else:
            payload = None

        self.write(
            {
                "ts": _now(),
                "seq": next(_seq),
                "layer": "cast",
                "direction": direction,
                "connection_id": connection_id,
                "source_id": msg.source_id,
                "destination_id": msg.destination_id,
                "namespace": msg.namespace,
                "payload_type": "string"
                if msg.payload_type == CastMessage.STRING
                else "binary",
                "payload": payload,
            }
        )


# ---------------------------------------------------------------------------
# Message filtering
# ---------------------------------------------------------------------------

# Track first-occurrence state for dedup (same strategy as go-cast proxy).
_seen_receiver_status_with_apps = False
_seen_custom_data = False


def _should_log(msg: CastMessage, *, verbose: bool) -> bool:
    """Return True if *msg* should appear in the capture log."""
    global _seen_receiver_status_with_apps, _seen_custom_data  # noqa: PLW0603

    if verbose:
        return True

    ns = msg.namespace

    # Heartbeat is always noise.
    if ns == HEARTBEAT:
        return False

    # Binary namespaces (device auth) are always interesting.
    if msg.payload_type != CastMessage.STRING:
        return True

    payload = parse_json_payload(msg)
    if payload is None:
        return True

    msg_type = payload.get("type", "")
    response_type = payload.get("responseType", "")

    if response_type == "GET_APP_AVAILABILITY":
        return False

    if msg_type in ("PING", "PONG"):
        return False

    if msg_type == "GET_STATUS" and ns in (RECEIVER, MEDIA):
        return False

    if msg_type == "GET_APP_AVAILABILITY":
        return False

    if msg_type == "MULTIZONE_STATUS" or ns == MULTIZONE:
        return False

    # RECEIVER_STATUS: only log the first one that contains applications.
    if msg_type == "RECEIVER_STATUS":
        status = payload.get("status", {})
        if not isinstance(status, dict) or "applications" not in status:
            return False
        if _seen_receiver_status_with_apps:
            return False
        _seen_receiver_status_with_apps = True
        return True

    # MEDIA_STATUS with requestId=0 is unsolicited position-update noise.
    if msg_type == "MEDIA_STATUS":
        req_id = payload.get("requestId", 0)
        if req_id == 0:
            return False

    # CUSTOM_DATA: log first only.
    if msg_type == "CUSTOM_DATA":
        if _seen_custom_data:
            return False
        _seen_custom_data = True

    return True


# ---------------------------------------------------------------------------
# Payload helpers
# ---------------------------------------------------------------------------


def _now() -> str:
    return datetime.now(tz=UTC).isoformat()


def _try_parse_json(text: str) -> Any:
    try:
        return json.loads(text)
    except (json.JSONDecodeError, ValueError):
        return text


def _enum_name(enum_type: type[object], value: int) -> str:
    name_fn = getattr(enum_type, "Name", None)
    if callable(name_fn):
        with contextlib.suppress(ValueError):
            return str(name_fn(value))
    return str(value)


def _decode_binary_payload(msg: CastMessage) -> dict[str, Any]:
    """Best-effort decode of a binary Cast payload."""
    if msg.namespace == DEVICE_AUTH:
        dam = DeviceAuthMessage()
        try:
            _ = dam.ParseFromString(msg.payload_binary)
        except Exception:
            return {"_raw_bytes": len(msg.payload_binary)}

        if dam.HasField("challenge"):
            ch = dam.challenge
            return {
                "_decoded": "DeviceAuthMessage.challenge",
                "hash_algorithm": _enum_name(HashAlgorithm, ch.hash_algorithm),
                "signature_algorithm": _enum_name(
                    SignatureAlgorithm, ch.signature_algorithm
                ),
                "sender_nonce_length": len(ch.sender_nonce),
            }
        if dam.HasField("response"):
            resp = dam.response
            return {
                "_decoded": "DeviceAuthMessage.response",
                "hash_algorithm": _enum_name(HashAlgorithm, resp.hash_algorithm),
                "signature_algorithm": _enum_name(
                    SignatureAlgorithm, resp.signature_algorithm
                ),
                "intermediate_certs": len(resp.intermediate_certificate),
                "crl_bytes": len(resp.crl),
                "sender_nonce_length": len(resp.sender_nonce),
            }
        if dam.HasField("error"):
            return {
                "_decoded": "DeviceAuthMessage.error",
                "error_type": dam.error.error_type,
            }
        return {"_decoded": "DeviceAuthMessage", "_empty": True}

    return {"_raw_bytes": len(msg.payload_binary)}


# ---------------------------------------------------------------------------
# TLS helpers
# ---------------------------------------------------------------------------


def _build_server_ssl_context(bundle: CertificateBundle) -> ssl.SSLContext:
    """Server-side TLS context (presented to the Cast sender)."""
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    ctx.minimum_version = ssl.TLSVersion.TLSv1_2
    cert_path: Path | None = None
    key_path: Path | None = None
    try:
        with tempfile.NamedTemporaryFile(suffix=".pem", delete=False) as f:
            _ = f.write(bundle.peer_cert_pem)
            cert_path = Path(f.name)
        with tempfile.NamedTemporaryFile(suffix=".pem", delete=False) as f:
            _ = f.write(bundle.peer_key_pem)
            key_path = Path(f.name)
        ctx.load_cert_chain(certfile=str(cert_path), keyfile=str(key_path))
    finally:
        if cert_path is not None:
            cert_path.unlink(missing_ok=True)
        if key_path is not None:
            key_path.unlink(missing_ok=True)
    return ctx


def _build_client_ssl_context() -> ssl.SSLContext:
    """Client-side TLS context for connecting to the upstream Cast device.

    Cast senders never verify the receiver's certificate, so we disable
    all verification.
    """
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    ctx.minimum_version = ssl.TLSVersion.TLSv1_2
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    return ctx


# ---------------------------------------------------------------------------
# Proxy session (one per sender connection)
# ---------------------------------------------------------------------------


class ProxySession:
    """Bidirectional proxy between a sender and the upstream Cast device."""

    def __init__(
        self,
        connection_id: int,
        sender_reader: asyncio.StreamReader,
        sender_writer: asyncio.StreamWriter,
        upstream_reader: asyncio.StreamReader,
        upstream_writer: asyncio.StreamWriter,
        bundle: CertificateBundle,
        crl: bytes | None,
        log_writer: LogWriter,
        *,
        verbose: bool = False,
    ) -> None:
        self._id = connection_id
        self._s_reader = sender_reader
        self._s_writer = sender_writer
        self._u_reader = upstream_reader
        self._u_writer = upstream_writer
        self._bundle = bundle
        self._crl = crl
        self._log = log_writer
        self._verbose = verbose

    async def run(self) -> None:
        """Run both forwarding directions until one side disconnects."""
        s2u = asyncio.create_task(
            self._forward_sender_to_upstream(),
            name=f"conn-{self._id}-s2u",
        )
        u2s = asyncio.create_task(
            self._forward_upstream_to_sender(),
            name=f"conn-{self._id}-u2s",
        )
        tasks = {s2u, u2s}
        try:
            done, pending = await asyncio.wait(
                tasks,
                return_when=asyncio.FIRST_COMPLETED,
            )
            # Cancel the remaining direction.
            for task in pending:
                _ = task.cancel()
                with contextlib.suppress(asyncio.CancelledError):
                    await task
            # Re-raise exceptions from completed tasks.
            for task in done:
                exc = task.exception()
                if exc is not None:
                    raise exc
        except asyncio.CancelledError:
            # Shutdown: cancel both directions and suppress their errors.
            for task in tasks:
                _ = task.cancel()
                with contextlib.suppress(BaseException):
                    await task
        finally:
            self._s_writer.close()
            self._u_writer.close()
            with contextlib.suppress(OSError, ssl.SSLError):
                await self._s_writer.wait_closed()
            with contextlib.suppress(OSError, ssl.SSLError):
                await self._u_writer.wait_closed()

    # -- sender -> upstream -------------------------------------------------

    async def _forward_sender_to_upstream(self) -> None:
        while True:
            try:
                msg = await read_message(self._s_reader)
            except (
                asyncio.IncompleteReadError,
                FramingError,
                ConnectionResetError,
                BrokenPipeError,
            ):
                return

            # Device auth: handle locally, never forward to upstream.
            if msg.namespace == DEVICE_AUTH:
                if _should_log(msg, verbose=self._verbose):
                    self._log.cast("sender_to_proxy", msg, connection_id=self._id)
                await self._handle_device_auth(msg)
                continue

            if _should_log(msg, verbose=self._verbose):
                self._log.cast("sender_to_device", msg, connection_id=self._id)

            try:
                await write_message(self._u_writer, msg)
            except (FramingError, ConnectionResetError, BrokenPipeError, OSError):
                return

    # -- upstream -> sender -------------------------------------------------

    async def _forward_upstream_to_sender(self) -> None:
        while True:
            try:
                msg = await read_message(self._u_reader)
            except (
                asyncio.IncompleteReadError,
                FramingError,
                ConnectionResetError,
                BrokenPipeError,
            ):
                return

            if _should_log(msg, verbose=self._verbose):
                self._log.cast("device_to_sender", msg, connection_id=self._id)

            try:
                await write_message(self._s_writer, msg)
            except (FramingError, ConnectionResetError, BrokenPipeError, OSError):
                return

    # -- device auth --------------------------------------------------------

    async def _handle_device_auth(self, msg: CastMessage) -> None:
        """Respond to a device-auth challenge using the manifest certs."""
        challenge = DeviceAuthMessage()
        requested_hash = HashAlgorithm.SHA1
        requested_sig = SignatureAlgorithm.RSASSA_PKCS1v15
        if challenge.ParseFromString(msg.payload_binary) and challenge.HasField(
            "challenge"
        ):
            requested_hash = challenge.challenge.hash_algorithm
            requested_sig = challenge.challenge.signature_algorithm

        if requested_sig != SignatureAlgorithm.RSASSA_PKCS1v15:
            sig_name = _enum_name(SignatureAlgorithm, requested_sig)
            print(
                "  [conn "
                f"{self._id}] unsupported device auth signature algorithm: {sig_name}; "
                "sending auth error"
            )
            self._log.meta(
                "device_auth_error",
                connection_id=self._id,
                reason="unsupported_signature_algorithm",
                signature_algorithm=sig_name,
            )
            payload = build_auth_error()
        else:
            try:
                payload = build_auth_response(
                    self._bundle,
                    hash_algorithm=requested_hash,
                    crl=self._crl,
                )
            except ValueError:
                hash_name = _enum_name(HashAlgorithm, requested_hash)
                print(
                    "  [conn "
                    f"{self._id}] unsupported device auth hash algorithm: {hash_name}; "
                    "sending auth error"
                )
                self._log.meta(
                    "device_auth_error",
                    connection_id=self._id,
                    reason="unsupported_hash_algorithm",
                    hash_algorithm=hash_name,
                )
                payload = build_auth_error()

        resp_msg = CastMessage()
        resp_msg.protocol_version = CastMessage.CASTV2_1_0
        resp_msg.source_id = msg.destination_id
        resp_msg.destination_id = msg.source_id
        resp_msg.namespace = DEVICE_AUTH
        resp_msg.payload_type = CastMessage.BINARY
        resp_msg.payload_binary = payload

        try:
            await write_message(self._s_writer, resp_msg)
        except (FramingError, ConnectionResetError, BrokenPipeError, OSError):
            return

        if _should_log(resp_msg, verbose=self._verbose):
            self._log.cast("proxy_to_sender", resp_msg, connection_id=self._id)

        print(f"  [conn {self._id}] device auth completed")


# ---------------------------------------------------------------------------
# mitmproxy subprocess management
# ---------------------------------------------------------------------------


def _find_mitmdump() -> str | None:
    """Return the path to ``mitmdump`` if installed, else None."""
    import shutil

    return shutil.which("mitmdump")


def _start_mitmproxy(log_path: Path) -> subprocess.Popen[bytes] | None:
    """Spawn ``mitmdump --mode wireguard`` with our addon script.

    Returns the Popen handle, or None if mitmdump is not found.
    """
    mitmdump = _find_mitmdump()
    if mitmdump is None:
        print(
            "WARNING: mitmdump not found. Install mitmproxy to capture HTTP traffic.\n"
            + "  pip install mitmproxy   (or)   brew install mitmproxy\n"
            + "Continuing with Cast-only capture.",
        )
        return None

    addon_path = Path(__file__).parent / "_mitm_addon.py"
    if not addon_path.exists():
        print(f"WARNING: mitmproxy addon not found at {addon_path}")
        return None

    env = {**os.environ, "CAPTURE_LOG_PATH": str(log_path)}

    print(f"Starting mitmdump --mode wireguard -s {addon_path}")
    print("Configure WireGuard on the Cast device to route external")
    print("traffic through the mitmproxy endpoint shown below.\n")

    return subprocess.Popen(
        [mitmdump, "--mode", "wireguard", "-s", str(addon_path)],
        env=env,
        # Let mitmproxy's own output (WireGuard config, etc.) go to stderr.
        stdout=sys.stderr,
        stderr=sys.stderr,
    )


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def _parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(
        description="Cast protocol capture proxy for reverse-engineering providers.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=(
            "Examples:\n"
            "  # Cast-only capture:\n"
            "  uv run python scripts/capture_provider.py \\\n"
            "      --manifest manifest.json --upstream 192.168.2.6\n\n"
            "  # Full capture with HTTP interception:\n"
            "  uv run python scripts/capture_provider.py \\\n"
            "      --manifest manifest.json --upstream 192.168.2.6 --enable-mitm\n"
        ),
    )
    _ = p.add_argument(
        "--manifest", type=Path, required=True, help="Path to certificate manifest JSON"
    )
    _ = p.add_argument(
        "--upstream", required=True, help="Upstream Cast device IP address"
    )
    _ = p.add_argument(
        "--upstream-port",
        type=int,
        default=8009,
        help="Upstream Cast device port (default: 8009)",
    )
    _ = p.add_argument(
        "--port", type=int, default=8009, help="Local proxy listen port (default: 8009)"
    )
    _ = p.add_argument(
        "--output",
        type=Path,
        default=None,
        help="Output log file (default: capture_<timestamp>.jsonl)",
    )
    _ = p.add_argument(
        "--friendly-name",
        default="Cast Proxy",
        help="mDNS friendly name (default: Cast Proxy)",
    )
    _ = p.add_argument(
        "--device-model",
        default="SHIELD Android TV",
        help="Device model for mDNS TXT record",
    )
    _ = p.add_argument(
        "--device-id",
        default=None,
        help=(
            "Stable mDNS device ID. If omitted, capture_provider persists one at "
            "~/.vibecast/capture_proxy_device_id"
        ),
    )
    _ = p.add_argument(
        "--no-mdns", action="store_true", help="Disable mDNS advertisement"
    )
    _ = p.add_argument(
        "--enable-mitm",
        action="store_true",
        help="Start mitmdump for HTTP traffic capture",
    )
    _ = p.add_argument(
        "--verbose",
        action="store_true",
        help="Log all messages including heartbeats and status polls",
    )
    return p.parse_args()


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------


def _load_or_create_device_id(path: Path) -> str:
    path.parent.mkdir(parents=True, exist_ok=True)
    if path.exists():
        existing = path.read_text(encoding="utf-8").strip()
        if existing:
            return existing

    device_id = uuid4().hex
    _ = path.write_text(device_id, encoding="utf-8")
    return device_id


async def _run(args: argparse.Namespace) -> None:
    # -- Load certificate bundle --------------------------------------------
    print(f"Loading certificate manifest: {args.manifest}")
    bundle = CertificateStore.from_manifest(args.manifest).active_bundle

    # -- Fetch CRL ----------------------------------------------------------
    crl = bundle.crl
    if crl is None:
        print("Fetching Cast CRL from Google...")
        crl = await fetch_crl()
        print(f"  CRL fetched ({len(crl)} bytes)")
    else:
        print(f"  Using CRL from manifest ({len(crl)} bytes)")

    # -- Prepare log file ---------------------------------------------------
    if args.output is not None:
        log_path = args.output
    else:
        ts = datetime.now(tz=UTC).strftime("%Y%m%d_%H%M%S")
        log_path = Path(f"capture_{ts}.jsonl")

    log_writer = LogWriter(log_path)
    print(f"Capture log: {log_path}")

    log_writer.meta(
        "capture_start",
        upstream=f"{args.upstream}:{args.upstream_port}",
        proxy_port=args.port,
        mitm_enabled=args.enable_mitm,
        friendly_name=args.friendly_name,
        verbose=args.verbose,
    )

    # -- Start mitmproxy (optional) -----------------------------------------
    mitm_proc: subprocess.Popen[bytes] | None = None
    if args.enable_mitm:
        mitm_proc = _start_mitmproxy(log_path)

    # -- Build TLS contexts -------------------------------------------------
    server_ssl = _build_server_ssl_context(bundle)
    client_ssl = _build_client_ssl_context()

    # -- Connection state ---------------------------------------------------
    connection_counter = count(1)
    active_tasks: set[asyncio.Task[None]] = set()

    async def handle_sender(
        reader: asyncio.StreamReader,
        writer: asyncio.StreamWriter,
    ) -> None:
        # Track this handler task so shutdown can cancel it.
        task = asyncio.current_task()
        if task is not None:
            active_tasks.add(task)
            task.add_done_callback(active_tasks.discard)

        peername = writer.get_extra_info("peername")
        peer = f"{peername[0]}:{peername[1]}" if peername else "unknown"
        conn_id = next(connection_counter)

        print(f"  [conn {conn_id}] sender connected from {peer}")
        log_writer.meta("sender_connected", connection_id=conn_id, peer=peer)

        # Connect to upstream Cast device.
        try:
            u_reader, u_writer = await asyncio.open_connection(
                args.upstream,
                args.upstream_port,
                ssl=client_ssl,
            )
        except OSError as exc:
            print(f"  [conn {conn_id}] upstream connection failed: {exc}")
            log_writer.meta(
                "upstream_connect_failed",
                connection_id=conn_id,
                error=str(exc),
            )
            writer.close()
            return

        print(
            f"  [conn {conn_id}] upstream connected to {args.upstream}:{args.upstream_port}"
        )
        log_writer.meta(
            "upstream_connected",
            connection_id=conn_id,
            upstream=f"{args.upstream}:{args.upstream_port}",
        )

        session = ProxySession(
            connection_id=conn_id,
            sender_reader=reader,
            sender_writer=writer,
            upstream_reader=u_reader,
            upstream_writer=u_writer,
            bundle=bundle,
            crl=crl,
            log_writer=log_writer,
            verbose=args.verbose,
        )

        try:
            await session.run()
        except Exception as exc:
            print(f"  [conn {conn_id}] error: {exc}")
        finally:
            print(f"  [conn {conn_id}] disconnected")
            log_writer.meta("sender_disconnected", connection_id=conn_id)

    # -- Start TLS server ---------------------------------------------------
    server = await asyncio.start_server(
        handle_sender,
        host="",
        port=args.port,
        ssl=server_ssl,
    )
    addrs = [sock.getsockname() for sock in server.sockets]
    print(f"Cast proxy listening on {addrs}")

    # -- mDNS advertisement -------------------------------------------------
    advertisement: CastAdvertisement | None = None
    if not args.no_mdns:
        device_id = args.device_id or _load_or_create_device_id(_DEFAULT_DEVICE_ID_PATH)
        advertisement = CastAdvertisement(
            friendly_name=args.friendly_name,
            device_model=args.device_model,
            device_id=device_id,
            port=args.port,
            cert_digest=bundle.cert_digest_md5,
        )
        await advertisement.start()
        print(
            f'mDNS: advertising as "{args.friendly_name}" ({advertisement.service_name})'
        )

    # -- Wait for shutdown --------------------------------------------------
    stop = asyncio.Event()
    loop = asyncio.get_running_loop()

    for sig in (signal.SIGINT, signal.SIGTERM):
        loop.add_signal_handler(sig, stop.set)

    print("\nReady. Cast to the proxy from your sender device.")
    print("Press Ctrl+C to stop.\n")

    _ = await stop.wait()

    # -- Shutdown -----------------------------------------------------------
    print("\nShutting down...")

    # Stop accepting new connections.
    server.close()

    # Cancel all active connection handler tasks so wait_closed() can
    # complete (Python 3.12+ waits for handlers to finish).
    for task in list(active_tasks):
        _ = task.cancel()
    if active_tasks:
        _ = await asyncio.gather(*active_tasks, return_exceptions=True)

    await server.wait_closed()

    if advertisement is not None:
        await advertisement.stop()

    if mitm_proc is not None:
        print("Stopping mitmproxy...")
        mitm_proc.terminate()
        try:
            _ = mitm_proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            mitm_proc.kill()

    log_writer.meta("capture_end")
    log_writer.close()
    print(f"Capture saved to: {log_path}")


def main() -> None:
    args = _parse_args()
    with contextlib.suppress(KeyboardInterrupt):
        asyncio.run(_run(args))


if __name__ == "__main__":
    main()
