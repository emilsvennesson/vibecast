"""CLI entry point for running a vibecast receiver."""

from __future__ import annotations

import argparse
import asyncio
import contextlib
import logging
from dataclasses import replace
from pathlib import Path
from uuid import uuid4

from vibecast._config import load_config
from vibecast._security.certificate import CertificateStore
from vibecast.receiver import CastReceiver

_DEFAULT_DATA_DIR = Path.home() / ".vibecast"
_DEVICE_ID_FILE_NAME = "cast_receiver_device_id"


def _parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Run a vibecast receiver")
    _ = parser.add_argument(
        "--certs",
        type=Path,
        default=None,
        help="Path to certificate bundle JSON (overrides config)",
    )
    _ = parser.add_argument(
        "--data-dir",
        type=Path,
        default=_DEFAULT_DATA_DIR,
        help="Persistent data directory (contains config.toml)",
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


def _resolve_certs_path(configured_certs: str, *, data_dir: Path) -> Path:
    path = Path(configured_certs).expanduser()
    if path.is_absolute():
        return path
    return data_dir / path


def _load_certificate_store(certs_path: Path) -> CertificateStore:
    if not certs_path.exists():
        msg = (
            "certificate bundle file not found: "
            f"{certs_path} (set [device].certs or pass --certs)"
        )
        raise RuntimeError(msg)
    return CertificateStore.from_manifest(certs_path)


async def _run(args: argparse.Namespace) -> None:
    data_dir = args.data_dir
    config = load_config(data_dir)

    if args.certs is not None:
        config = replace(
            config,
            device=replace(config.device, certs=str(args.certs)),
        )

    configured_certs = config.device.certs.strip()
    if not configured_certs:
        msg = (
            "certificate bundle is not configured; set [device].certs in "
            f"{data_dir / 'config.toml'} or pass --certs"
        )
        raise RuntimeError(msg)

    certs_path = _resolve_certs_path(configured_certs, data_dir=data_dir)
    certificates = _load_certificate_store(certs_path)

    device_id_path = data_dir / _DEVICE_ID_FILE_NAME
    device_id = _load_or_create_device_id(device_id_path)
    receiver = CastReceiver(
        config=config,
        certificates=certificates,
        device_id=device_id,
        data_dir=data_dir,
    )
    await receiver.run_forever()


def main() -> None:
    args = _parse_args()
    _configure_logging(args.log_level)
    with contextlib.suppress(KeyboardInterrupt):
        asyncio.run(_run(args))


if __name__ == "__main__":
    main()
