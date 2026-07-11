"""Developer capture tool for reverse-engineering Google Cast apps.

Sits between a Cast sender (phone) and a genuine receiver (e.g. an NVIDIA
Shield), relaying every CastV2 message and logging it to ``cast.jsonl``. In
parallel it runs mitmproxy (as a library, in WireGuard mode) so the receiver's
decrypted HTTP/HTTPS egress is logged to ``http.jsonl``. Merge the two streams
by ``ts`` to see the whole picture of how an app talks to its backend.

The Cast protocol pieces (certificate bundle, device-auth reply, framing,
payload decode, mDNS advertisement) come from the ``vibecast-primitives-ffi``
Rust crate via UniFFI — this tool contains no re-implementation of them.

Usage::

    uv run capture.py --certs ~/.vibecast/certs.json --upstream 192.168.2.6

See README.md for one-time setup (WireGuard tunnel + mitmproxy CA on the
device). Ctrl-C stops the session.
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

# The UniFFI Python bindings + cdylib are generated into ./generated (see
# README.md / regenerate-bindings.sh); make them importable.
sys.path.insert(0, str(Path(__file__).parent / "generated"))
try:
    import vibecast_primitives_ffi as vc
except ImportError as exc:  # pragma: no cover - setup guard
    sys.exit(
        f"Could not import the vibecast primitives bindings ({exc}).\n"
        "Generate them first:  ./regenerate-bindings.sh   (see README.md)."
    )

from mitmproxy import options  # noqa: E402
from mitmproxy.tools.dump import DumpMaster  # noqa: E402

from mitm_addon import HttpJsonl  # noqa: E402

DEVICE_AUTH = "urn:x-cast:com.google.cast.tp.deviceauth"
DISCOVERY = "urn:x-cast:com.google.cast.receiver.discovery"
RECEIVER = "urn:x-cast:com.google.cast.receiver"


# ---------------------------------------------------------------------------
# JSONL recorder (cast stream)
# ---------------------------------------------------------------------------


def _cast_label(payload: Any) -> str:
    """A concise one-line label for a decoded Cast payload (console output)."""
    if isinstance(payload, dict):
        return (
            payload.get("type")
            or payload.get("responseType")
            or payload.get("_decoded")
            or ",".join(payload.keys())
            or "{}"
        )
    if isinstance(payload, str):
        return payload[:40]
    return "" if payload is None else str(payload)


class CastLog:
    """Append-only ``cast.jsonl`` writer with a monotonic sequence."""

    def __init__(self, path: Path) -> None:
        self._file = path.open("a", buffering=1, encoding="utf-8")
        self._seq = count(1)

    def close(self) -> None:
        with contextlib.suppress(Exception):
            self._file.close()

    def _write(self, entry: dict[str, Any]) -> None:
        now = datetime.now(tz=UTC)
        record = {
            "seq": next(self._seq),
            "ts": now.isoformat(),
            "ts_unix_ms": int(now.timestamp() * 1000),
            **entry,
        }
        self._file.write(json.dumps(record, ensure_ascii=False, default=str) + "\n")

    def meta(self, event: str, **fields: Any) -> None:
        self._write({"layer": "meta", "event": event, **fields})

    def cast(self, conn_id: int, direction: str, msg: Any) -> None:
        payload_type = "binary" if msg.payload_type == vc.PayloadType.BINARY else "string"
        payload = json.loads(vc.decode_payload_json(msg))
        self._write(
            {
                "layer": "cast",
                "direction": direction,
                "connection_id": conn_id,
                "source_id": msg.source_id,
                "destination_id": msg.destination_id,
                "namespace": msg.namespace,
                "payload_type": payload_type,
                "payload": payload,
            }
        )
        ns = msg.namespace.removeprefix("urn:x-cast:")
        print(f"  cast  {direction:<16} {ns:<28} {_cast_label(payload)}", flush=True)


# ---------------------------------------------------------------------------
# TLS contexts
# ---------------------------------------------------------------------------


def build_server_ctx(bundle: Any) -> ssl.SSLContext:
    """Server context presenting the harvested peer certificate to senders."""
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    ctx.minimum_version = ssl.TLSVersion.TLSv1_2
    cert_path = key_path = None
    try:
        with tempfile.NamedTemporaryFile("wb", suffix=".pem", delete=False) as f:
            f.write(bundle.peer_cert_pem())
            cert_path = f.name
        with tempfile.NamedTemporaryFile("wb", suffix=".pem", delete=False) as f:
            f.write(bundle.peer_key_pem())
            key_path = f.name
        ctx.load_cert_chain(certfile=cert_path, keyfile=key_path)
    finally:
        for p in (cert_path, key_path):
            if p:
                with contextlib.suppress(OSError):
                    os.unlink(p)
    return ctx


def build_client_ctx() -> ssl.SSLContext:
    """Client context for the upstream leg (senders never verify receivers)."""
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    ctx.minimum_version = ssl.TLSVersion.TLSv1_2
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    return ctx


# ---------------------------------------------------------------------------
# Cast MITM proxy
# ---------------------------------------------------------------------------


async def _frames(reader: asyncio.StreamReader):
    """Yield decoded CastMessages from a stream using the FFI framing."""
    buf = bytearray()
    while True:
        chunk = await reader.read(65536)
        if not chunk:
            return
        buf += chunk
        while True:
            parsed = vc.try_parse_frame(bytes(buf))
            if parsed is None:
                break
            del buf[: parsed.consumed]
            yield parsed.message


def _parse_obj(payload_utf8: str) -> dict[str, Any] | None:
    """Parse a string payload as a JSON object, or None if it isn't one."""
    try:
        data = json.loads(payload_utf8)
    except json.JSONDecodeError:
        return None
    return data if isinstance(data, dict) else None


def _with_payload(msg: Any, data: dict[str, Any]) -> Any:
    """Return a copy of `msg` carrying `data` as its (string) JSON payload."""
    return vc.CastMessage(
        protocol_version=msg.protocol_version,
        source_id=msg.source_id,
        destination_id=msg.destination_id,
        namespace=msg.namespace,
        payload_type=vc.PayloadType.STRING,
        payload_utf8=json.dumps(data),
        payload_binary=None,
    )


class CastProxy:
    def __init__(
        self,
        bundle: Any,
        upstream: str,
        upstream_port: int,
        log: CastLog,
        device_info: dict[str, Any] | None = None,
        available_apps: set[str] | None = None,
        all_apps_available: bool = False,
    ) -> None:
        self._bundle = bundle
        self._upstream = upstream
        self._upstream_port = upstream_port
        self._log = log
        # DEVICE_INFO field overrides applied to device->sender messages so
        # senders see a spoofed model/name/capabilities while the real receiver
        # still handles everything (lets apps that block "unsupported" platforms
        # cast anyway).
        self._device_info = device_info or {}
        # App IDs to force APP_AVAILABLE in GET_APP_AVAILABILITY replies (some
        # senders hide the device for an app the receiver reports unavailable).
        self._available_apps = available_apps or set()
        self._all_apps_available = all_apps_available
        self._client_ctx = build_client_ctx()
        self._conn_ids = count(1)

    async def handle_sender(
        self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter
    ) -> None:
        conn_id = next(self._conn_ids)
        peer = writer.get_extra_info("peername")
        self._log.meta("sender_connected", connection_id=conn_id, peer=str(peer))
        try:
            up_reader, up_writer = await asyncio.open_connection(
                self._upstream, self._upstream_port, ssl=self._client_ctx
            )
        except OSError as exc:
            self._log.meta("upstream_connect_failed", connection_id=conn_id, error=str(exc))
            writer.close()
            return

        s2u = asyncio.create_task(self._sender_to_upstream(conn_id, reader, writer, up_writer))
        u2s = asyncio.create_task(self._upstream_to_sender(conn_id, up_reader, writer))
        try:
            await asyncio.wait({s2u, u2s}, return_when=asyncio.FIRST_COMPLETED)
        finally:
            for task in (s2u, u2s):
                task.cancel()
                with contextlib.suppress(asyncio.CancelledError, Exception):
                    await task
            for w in (writer, up_writer):
                w.close()
                with contextlib.suppress(OSError, ssl.SSLError):
                    await w.wait_closed()
            self._log.meta("sender_disconnected", connection_id=conn_id)

    async def _sender_to_upstream(self, conn_id, reader, writer, up_writer) -> None:
        async for msg in _frames(reader):
            if msg.namespace == DEVICE_AUTH:
                # Answer device auth locally with the harvested signature; the
                # real receiver's response binds *its* cert and can't be relayed.
                self._log.cast(conn_id, "sender_to_proxy", msg)
                reply = self._bundle.device_auth_reply(msg)
                self._log.cast(conn_id, "proxy_to_sender", reply)
                writer.write(vc.serialize_frame(reply))
                await writer.drain()
            else:
                self._log.cast(conn_id, "sender_to_device", msg)
                up_writer.write(vc.serialize_frame(msg))
                await up_writer.drain()

    async def _upstream_to_sender(self, conn_id, up_reader, writer) -> None:
        async for msg in _frames(up_reader):
            self._log.cast(conn_id, "device_to_sender", msg)
            out = self._rewrite_device_info(conn_id, msg)
            out = self._rewrite_app_availability(conn_id, out)
            writer.write(vc.serialize_frame(out))
            await writer.drain()

    def _rewrite_device_info(self, conn_id: int, msg: Any) -> Any:
        """Apply DEVICE_INFO field overrides to a device->sender message.

        Senders read `deviceModel`/`friendlyName`/`deviceCapabilities` from the
        `DEVICE_INFO` reply to decide whether the platform is "supported"; this
        rewrites them in flight (the real receiver still runs the app).
        """
        if not self._device_info or msg.namespace != DISCOVERY or not msg.payload_utf8:
            return msg
        data = _parse_obj(msg.payload_utf8)
        if data is None or data.get("type") != "DEVICE_INFO":
            return msg

        changes = {k: v for k, v in self._device_info.items() if data.get(k) != v}
        if not changes:
            return msg
        data.update(changes)
        self._log.meta("device_info_rewritten", connection_id=conn_id, changes=changes)
        print(
            "  edit  DEVICE_INFO  " + ", ".join(f"{k}={v!r}" for k, v in changes.items()),
            flush=True,
        )
        return _with_payload(msg, data)

    def _rewrite_app_availability(self, conn_id: int, msg: Any) -> Any:
        """Force GET_APP_AVAILABILITY replies to report apps as APP_AVAILABLE.

        Senders query app availability before offering the device for an app
        (e.g. Netflix's `CA5E8412`); if the receiver reports `APP_UNAVAILABLE`
        the sender hides the device. This flips the queried apps to available so
        the sender offers the cast (the LAUNCH is still relayed upstream).
        """
        if not self._all_apps_available and not self._available_apps:
            return msg
        if msg.namespace != RECEIVER or not msg.payload_utf8:
            return msg
        data = _parse_obj(msg.payload_utf8)
        if data is None or data.get("responseType") != "GET_APP_AVAILABILITY":
            return msg
        availability = data.get("availability")
        if not isinstance(availability, dict):
            return msg

        changed = {}
        for app_id, status in availability.items():
            if (self._all_apps_available or app_id in self._available_apps) and status != "APP_AVAILABLE":
                changed[app_id] = status
                availability[app_id] = "APP_AVAILABLE"
        if not changed:
            return msg
        self._log.meta("app_availability_rewritten", connection_id=conn_id, changed=changed)
        print(
            "  edit  APP_AVAILABILITY  "
            + ", ".join(f"{a}:{s}->APP_AVAILABLE" for a, s in changed.items()),
            flush=True,
        )
        return _with_payload(msg, data)


# ---------------------------------------------------------------------------
# WireGuard tunnel status (read-only; the user enables/disables the tunnel)
# ---------------------------------------------------------------------------


class Tunnel:
    """Read-only view of the on-device WireGuard tunnel that routes egress to
    mitmproxy. This tool never enables, disables, or otherwise changes the
    tunnel — the user controls it in the WireGuard app. Detection is a best-effort
    convenience and requires adb + root; capture works regardless.
    """

    def __init__(self, tunnel: str, tunnel_ip: str, serial: str | None) -> None:
        self.tunnel = tunnel
        self.tunnel_ip = tunnel_ip
        self.serial = serial

    def _adb(self, *args: str) -> subprocess.CompletedProcess[str]:
        cmd = ["adb"] + (["-s", self.serial] if self.serial else []) + list(args)
        return subprocess.run(cmd, capture_output=True, text=True, timeout=15, check=False)

    def adb_available(self) -> bool:
        try:
            return self._adb("get-state").stdout.strip() == "device"
        except (OSError, subprocess.SubprocessError):
            return False

    def is_up(self) -> bool:
        """Whether the tunnel interface is present (read-only; may need root)."""
        try:
            out = self._adb("shell", "su", "-c", "ip -o addr show").stdout
        except (OSError, subprocess.SubprocessError):
            return False
        return self.tunnel_ip in out


def print_tunnel_banner(tunnel: str, mac_ip: str, wg_port: int) -> None:
    print(
        "\n"
        "  ┌─ HTTP/HTTPS capture requires the WireGuard tunnel to be ON ───────────\n"
        f"  │ Enable tunnel '{tunnel}' yourself in the WireGuard app on the device.\n"
        f"  │ It routes the receiver's egress to mitmproxy at {mac_ip}:{wg_port}.\n"
        "  │ This tool never toggles the tunnel — you control it.\n"
        "  │ Requirements: mitmproxy CA trusted in the device SYSTEM store, and\n"
        "  │ the tunnel's AllowedIPs must exclude this LAN so Cast/mDNS stay local.\n"
        "  └───────────────────────────────────────────────────────────────────────\n"
    )


def report_tunnel(tunnel: Tunnel, mac_ip: str, wg_port: int) -> None:
    """Print the tunnel requirement and a read-only status hint. Never mutates."""
    print_tunnel_banner(tunnel.tunnel, mac_ip, wg_port)
    if not tunnel.adb_available():
        print("  · adb not reachable; make sure you have enabled the tunnel.")
        return
    if tunnel.is_up():
        print(f"  ✓ WireGuard tunnel '{tunnel.tunnel}' detected up.")
    else:
        print(
            f"  · Tunnel '{tunnel.tunnel}' not detected up — HTTP will be recorded "
            "once you enable it."
        )


# ---------------------------------------------------------------------------
# mitmproxy (library mode)
# ---------------------------------------------------------------------------


def build_master(http_path: Path, wg_port: int) -> tuple[DumpMaster, HttpJsonl]:
    opts = options.Options(mode=[f"wireguard@{wg_port}"])
    master = DumpMaster(opts, with_termlog=False, with_dumper=False)
    addon = HttpJsonl(http_path)
    master.addons.add(addon)
    return master, addon


# ---------------------------------------------------------------------------
# CLI + main
# ---------------------------------------------------------------------------


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("--certs", type=Path, required=True, help="Certificate manifest JSON path")
    p.add_argument("--upstream", required=True, help="Genuine receiver IP/host to relay to")
    p.add_argument("--upstream-port", type=int, default=8009)
    p.add_argument("--listen-port", type=int, default=8009, help="Local port senders connect to")
    p.add_argument("--out", type=Path, default=Path("captures"), help="Session output dir root")
    p.add_argument("--name", default=None, help="Session name (default: UTC timestamp)")
    p.add_argument(
        "--friendly-name",
        default=None,
        help="Device name shown to senders — mDNS 'fn' AND DEVICE_INFO friendlyName "
        "(default: the upstream host)",
    )
    p.add_argument(
        "--model",
        default="SHIELD Android TV",
        help="Device model shown to senders — mDNS 'md' AND DEVICE_INFO deviceModel. "
        "Set e.g. 'Chromecast' so apps that hide other models still discover it.",
    )
    p.add_argument("--device-id", default=None, help="Stable device id (default: persisted)")
    p.add_argument("--tunnel", default="wg_mitm", help="WireGuard tunnel name on the device")
    p.add_argument("--tunnel-ip", default="10.0.0.1", help="Tunnel interface IP (for up-detection)")
    p.add_argument("--wg-port", type=int, default=51820, help="mitmproxy WireGuard listen port")
    p.add_argument("--adb-serial", default=None, help="Target adb serial (multi-device hosts)")
    p.add_argument("--local-ip", default=None, help="This host's LAN IP (default: auto-detected)")
    p.add_argument("--no-http", action="store_true", help="Cast-only; skip mitmproxy + WireGuard")
    p.add_argument(
        "--spoof",
        action="append",
        default=[],
        metavar="KEY=VALUE",
        help="Override any additional DEVICE_INFO field (repeatable; value parsed as "
        "JSON, else string), e.g. --spoof deviceCapabilities=2115",
    )
    p.add_argument(
        "--available",
        action="append",
        default=[],
        metavar="APPID",
        help="Force GET_APP_AVAILABILITY to report this app id as APP_AVAILABLE "
        "(repeatable), e.g. --available CA5E8412 (Netflix)",
    )
    p.add_argument(
        "--all-available",
        action="store_true",
        help="Report every app queried via GET_APP_AVAILABILITY as APP_AVAILABLE",
    )
    return p.parse_args()


def build_device_info(args: argparse.Namespace, device_id: str, friendly_name: str) -> dict[str, Any]:
    """The coherent identity the proxy presents in DEVICE_INFO.

    It uses the proxy's *own* deviceId/model/name (matching its mDNS
    advertisement) rather than passing the real receiver's through — otherwise
    senders see the upstream's deviceId and merge the proxy with the real device
    (flip-flopping between the two names). Any `--spoof KEY=VALUE` extras are
    layered on top.
    """
    info: dict[str, Any] = {
        "deviceId": device_id,
        "deviceModel": args.model,
        "friendlyName": friendly_name,
    }
    for item in args.spoof:
        key, sep, raw = item.partition("=")
        if not sep:
            sys.exit(f"--spoof expects KEY=VALUE, got: {item!r}")
        try:
            value: Any = json.loads(raw)
        except json.JSONDecodeError:
            value = raw
        info[key] = value
    return info


def local_ip_towards(target: str) -> str:
    import socket

    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        s.connect((target, 9))
        return s.getsockname()[0]
    except OSError:
        return "127.0.0.1"
    finally:
        s.close()


def load_or_create_device_id(path: Path) -> str:
    path.parent.mkdir(parents=True, exist_ok=True)
    if path.exists():
        existing = path.read_text(encoding="utf-8").strip()
        if existing:
            return existing
    from uuid import uuid4

    device_id = uuid4().hex
    path.write_text(device_id, encoding="utf-8")
    return device_id


async def amain(args: argparse.Namespace) -> None:
    bundle = vc.CertBundle.load(str(args.certs))

    name = args.name or datetime.now(tz=UTC).strftime("%Y%m%d_%H%M%S")
    session_dir = args.out / name
    session_dir.mkdir(parents=True, exist_ok=True)
    cast_log = CastLog(session_dir / "cast.jsonl")
    http_path = session_dir / "http.jsonl"
    print(f"Capture session: {session_dir}")

    mac_ip = args.local_ip or local_ip_towards(args.upstream)
    friendly_name = args.friendly_name or args.upstream
    device_id = args.device_id or load_or_create_device_id(args.out / "device_id")

    cast_log.meta(
        "capture_start",
        upstream=f"{args.upstream}:{args.upstream_port}",
        listen_port=args.listen_port,
        capture_http=not args.no_http,
        cert_digest=bundle.cert_digest_md5(),
    )

    advertiser = vc.CastAdvertiser.start(
        friendly_name, args.model, device_id, args.listen_port, bundle.cert_digest_md5()
    )
    print(f'mDNS: advertising "{friendly_name}" (model {args.model})')

    device_info = build_device_info(args, device_id, friendly_name)
    cast_log.meta("device_info_identity", identity=device_info)
    print(f"Presenting as: model={args.model!r}, name={friendly_name!r}, deviceId={device_id}")

    available_apps = set(args.available)
    if args.all_available:
        print("App availability: forcing ALL queried apps to APP_AVAILABLE")
    elif available_apps:
        print(f"App availability: forcing APP_AVAILABLE for {sorted(available_apps)}")

    proxy = CastProxy(
        bundle,
        args.upstream,
        args.upstream_port,
        cast_log,
        device_info,
        available_apps,
        args.all_available,
    )
    server = await asyncio.start_server(
        proxy.handle_sender, host="0.0.0.0", port=args.listen_port, ssl=build_server_ctx(bundle)
    )

    master: DumpMaster | None = None
    addon: HttpJsonl | None = None
    master_task: asyncio.Task[None] | None = None
    if not args.no_http:
        master, addon = build_master(http_path, args.wg_port)
        master_task = asyncio.create_task(master.run())
        report_tunnel(Tunnel(args.tunnel, args.tunnel_ip, args.adb_serial), mac_ip, args.wg_port)

    print(f'\nReady. Cast to "{friendly_name}" from your sender. Ctrl-C to stop.\n')

    stop = asyncio.Event()
    loop = asyncio.get_running_loop()
    for sig in (signal.SIGINT, signal.SIGTERM):
        with contextlib.suppress(NotImplementedError):
            loop.add_signal_handler(sig, stop.set)
    try:
        await stop.wait()
    except KeyboardInterrupt:
        pass

    print("\nShutting down…")
    server.close()
    with contextlib.suppress(Exception):
        await server.wait_closed()
    if master is not None:
        master.shutdown()
    if master_task is not None:
        with contextlib.suppress(asyncio.CancelledError, Exception):
            await master_task
    if addon is not None:
        addon.done()
    advertiser.stop()
    cast_log.meta("capture_end")
    cast_log.close()
    print(f"Saved to {session_dir}")


def main() -> None:
    args = parse_args()
    with contextlib.suppress(KeyboardInterrupt):
        asyncio.run(amain(args))


if __name__ == "__main__":
    main()
