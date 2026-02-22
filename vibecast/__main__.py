"""CLI entry point for running a vibecast receiver."""

from __future__ import annotations

import argparse
import asyncio
import contextlib
import logging
from pathlib import Path
from uuid import uuid4

from vibecast._certificate import CertificateBundle
from vibecast.receiver import CastReceiver, ReceiverConfig

_DEFAULT_DATA_DIR = Path.home() / ".vibecast"
_DEVICE_ID_FILE_NAME = "cast_receiver_device_id"


def _parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Run a vibecast receiver")
    _ = parser.add_argument(
        "--manifest",
        type=Path,
        required=True,
        help="Path to go-cast compatible certificate manifest JSON",
    )
    _ = parser.add_argument(
        "--name",
        required=True,
        help="Friendly receiver name advertised over mDNS",
    )
    _ = parser.add_argument(
        "--model",
        default="Chromecast",
        help="Device model string advertised via mDNS",
    )
    _ = parser.add_argument(
        "--device-id",
        default=None,
        help=(
            "Stable device ID for mDNS/discovery. "
            "If omitted, vibecast persists one at ~/.vibecast/cast_receiver_device_id"
        ),
    )
    _ = parser.add_argument(
        "--data-dir",
        type=Path,
        default=_DEFAULT_DATA_DIR,
        help="Persistent receiver data (cookies, device IDs, provider state)",
    )
    _ = parser.add_argument(
        "--bind-host",
        default="0.0.0.0",
        help="Host/interface to bind Cast, eureka, and player servers to",
    )
    _ = parser.add_argument(
        "--player-port",
        type=int,
        default=8010,
        help="Port to bind the player HTTP/WebSocket server to",
    )
    _ = parser.add_argument(
        "--log-level",
        default="INFO",
        choices=("DEBUG", "INFO", "WARNING", "ERROR"),
        help="Logging verbosity",
    )
    return parser.parse_args()


def _configure_logging(level: str) -> None:
    logging.basicConfig(
        level=getattr(logging, level.upper(), logging.INFO),
        format="%(asctime)s %(levelname)s %(name)s: %(message)s",
    )


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
    bundle = CertificateBundle.from_manifest(args.manifest)
    data_dir = args.data_dir
    device_id_path = data_dir / _DEVICE_ID_FILE_NAME
    device_id = args.device_id or _load_or_create_device_id(device_id_path)
    config = ReceiverConfig(
        friendly_name=args.name,
        device_model=args.model,
        device_id=device_id,
        bind_host=args.bind_host,
        player_port=args.player_port,
        data_dir=data_dir,
    )
    receiver = CastReceiver(config=config, certificates=bundle)
    await receiver.run_forever()


def main() -> None:
    args = _parse_args()
    _configure_logging(args.log_level)
    with contextlib.suppress(KeyboardInterrupt):
        asyncio.run(_run(args))


if __name__ == "__main__":
    main()
