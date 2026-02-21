"""Public Cast receiver orchestrator."""

from __future__ import annotations

import asyncio
from dataclasses import dataclass, field
from pathlib import Path
from typing import TYPE_CHECKING
from uuid import uuid4

from castvibe._auth import fetch_crl
from castvibe._device import Device, DeviceIdentity
from castvibe._discovery import CastAdvertisement
from castvibe._handlers import PlatformHandler
from castvibe._http import ReceiverHTTPClient
from castvibe._log import get_logger
from castvibe._server import CastServer
from castvibe.provider import Provider, ProviderRegistry, discover_providers

if TYPE_CHECKING:
    from castvibe._certificate import CertificateBundle
    from castvibe._connection import Connection
    from castvibe._proto.cast_channel_pb2 import CastMessage

log = get_logger("receiver")


@dataclass(slots=True)
class ReceiverConfig:
    """Public configuration for ``CastReceiver``."""

    friendly_name: str
    device_model: str = "Chromecast"
    device_id: str | None = None
    host: str = "0.0.0.0"
    port: int = 8009
    data_dir: Path = field(default_factory=lambda: Path.home() / ".castvibe")


class CastReceiver:
    """High-level receiver API wiring server, routing, and mDNS."""

    __slots__ = (
        "_advertisement",
        "_certificates",
        "_http",
        "_server",
        "_started",
        "_stop_event",
        "config",
        "device",
        "providers",
    )

    def __init__(
        self,
        config: ReceiverConfig,
        certificates: CertificateBundle,
        providers: list[Provider] | None = None,
    ) -> None:
        device_id = config.device_id or str(uuid4())
        config.device_id = device_id

        self.config = config
        self._certificates = certificates
        self._http = ReceiverHTTPClient(data_dir=config.data_dir)
        self.providers = ProviderRegistry(
            discover_providers() if providers is None else providers
        )

        self.device = Device(
            DeviceIdentity(
                friendly_name=config.friendly_name,
                device_model=config.device_model,
                device_id=device_id,
            ),
            http_client=self._http.client,
            data_dir=config.data_dir,
        )

        self.device.register_transport(
            "receiver-0",
            PlatformHandler(self.device, provider_lookup=self.providers.get),
        )

        self._server = CastServer(
            certificates,
            host=config.host,
            port=config.port,
            on_message=self._on_message,
            on_disconnect=self._on_disconnect,
        )
        self._advertisement: CastAdvertisement | None = None
        self._started = False
        self._stop_event = asyncio.Event()

    @property
    def serving_port(self) -> int | None:
        """Current listening TCP port (after ``start()``)."""
        return self._server.serving_port

    async def start(self) -> None:
        """Start TLS server and mDNS advertisement."""
        if self._started:
            return

        if self._http.client.is_closed:
            self._http = ReceiverHTTPClient(data_dir=self.config.data_dir)
            self.device.set_http_client(self._http.client)

        crl = self._certificates.crl
        if crl is None:
            try:
                crl = await fetch_crl(client=self._http.client)
                log.info("fetched Cast CRL (%d bytes)", len(crl))
            except Exception as exc:
                msg = "failed to fetch Cast CRL"
                raise RuntimeError(msg) from exc
        else:
            log.info("using CRL from manifest (%d bytes)", len(crl))
        self._server.crl = crl

        await self._server.start()
        port = self._server.serving_port
        if port is None:
            await self._server.stop()
            msg = "server did not expose serving port"
            raise RuntimeError(msg)

        device_id = self.config.device_id
        if device_id is None:
            await self._server.stop()
            msg = "receiver device_id is not initialized"
            raise RuntimeError(msg)

        advertisement = CastAdvertisement(
            friendly_name=self.config.friendly_name,
            device_model=self.config.device_model,
            device_id=device_id,
            port=port,
            cert_digest=self._certificates.cert_digest_md5,
        )
        try:
            await advertisement.start()
        except Exception:
            await self._server.stop()
            raise

        self._advertisement = advertisement
        self._started = True
        self._stop_event.clear()
        log.info(
            "cast receiver started (name=%s, host=%s, port=%d, service=%s, addresses=%s)",
            self.config.friendly_name,
            self.config.host,
            port,
            advertisement.service_name,
            ",".join(advertisement.parsed_addresses),
        )

    async def stop(self) -> None:
        """Stop sessions, mDNS advertisement, and TLS server.

        Safe to call multiple times; subsequent calls are no-ops.
        """
        if not self._started:
            await self._http.close()
            return
        self._stop_event.set()

        for session_id in self.device.session_ids():
            _ = await self.device.stop_session(session_id)

        advertisement = self._advertisement
        self._advertisement = None
        try:
            if advertisement is not None:
                await advertisement.stop()
        finally:
            await self._server.stop()
            await self._http.close()
            self._started = False
            log.info("cast receiver stopped")

    async def run_forever(self) -> None:
        """Start receiver and wait until ``stop()`` or cancellation."""
        await self.start()
        try:
            _ = await self._stop_event.wait()
        finally:
            await self.stop()

    async def _on_message(self, connection: Connection, msg: CastMessage) -> None:
        await self.device.route_message(connection, msg)

    async def _on_disconnect(self, connection: Connection) -> None:
        _ = self.device.remove_all_subscriptions(connection)


__all__ = ["CastReceiver", "ReceiverConfig"]
