"""CLI entry point for running a castvibe receiver."""

from __future__ import annotations

import argparse
import asyncio
import contextlib
import logging
from pathlib import Path

from castvibe._certificate import CertificateBundle
from castvibe.receiver import CastReceiver, ReceiverConfig


def _parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Run a castvibe receiver")
    parser.add_argument(
        "--manifest",
        type=Path,
        required=True,
        help="Path to go-cast compatible certificate manifest JSON",
    )
    parser.add_argument(
        "--name",
        required=True,
        help="Friendly receiver name advertised over mDNS",
    )
    parser.add_argument(
        "--model",
        default="Chromecast",
        help="Device model string advertised via mDNS",
    )
    parser.add_argument(
        "--host",
        default="0.0.0.0",
        help="Host/interface to bind the TLS Cast server to",
    )
    parser.add_argument(
        "--port",
        type=int,
        default=8009,
        help="Port to bind the TLS Cast server to",
    )
    parser.add_argument(
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


async def _run(args: argparse.Namespace) -> None:
    bundle = CertificateBundle.from_manifest(args.manifest)
    config = ReceiverConfig(
        friendly_name=args.name,
        device_model=args.model,
        host=args.host,
        port=args.port,
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
