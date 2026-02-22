"""Public Cast receiver orchestrator."""

from __future__ import annotations

import asyncio
from dataclasses import dataclass, field
from pathlib import Path
from typing import TYPE_CHECKING, Final
from uuid import uuid4

from vibecast._auth import fetch_crl
from vibecast._device import Device, DeviceIdentity
from vibecast._discovery import CastAdvertisement
from vibecast._eureka import EurekaIdentity, EurekaServer
from vibecast._handlers import PlatformHandler
from vibecast._http import ReceiverHTTPClient
from vibecast._log import get_logger
from vibecast._player_server import PlayerServer
from vibecast._server import CastServer
from vibecast.provider import Provider, ProviderRegistry, discover_providers

if TYPE_CHECKING:
    from vibecast._certificate import CertificateBundle
    from vibecast._connection import Connection
    from vibecast._proto.cast_channel_pb2 import CastMessage

log = get_logger("receiver")

_CAST_PORT: Final[int] = 8009
_EUREKA_HTTPS_PORT: Final[int] = 8443
_EUREKA_HTTP_PORT: Final[int] = 8008
_DISCOVERY_BASE_APP_IDS: Final[frozenset[str]] = frozenset({"CC1AD845", "0F5096E8"})


@dataclass(slots=True)
class ReceiverConfig:
    """Public configuration for ``CastReceiver``."""

    friendly_name: str
    device_model: str = "Chromecast"
    device_id: str | None = None
    bind_host: str = "0.0.0.0"
    player_port: int = 8010
    data_dir: Path = field(default_factory=lambda: Path.home() / ".vibecast")


class CastReceiver:
    """High-level receiver API wiring server, routing, and mDNS."""

    __slots__ = (
        "_advertisement",
        "_certificates",
        "_eureka_server",
        "_http",
        "_player_server",
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
            get_http_client=lambda: self._http.client,
            data_dir=config.data_dir,
        )

        self._player_server = PlayerServer(
            host=config.bind_host,
            port=config.player_port,
        )

        self._eureka_server = EurekaServer(
            certificates,
            EurekaIdentity(
                friendly_name=config.friendly_name,
                device_model=config.device_model,
                ssdp_udn=device_id,
            ),
            host=config.bind_host,
            https_port=_EUREKA_HTTPS_PORT,
            http_port=_EUREKA_HTTP_PORT,
        )

        self.device.register_transport(
            "receiver-0",
            PlatformHandler(
                self.device,
                player=self._player_server,
                player_server=self._player_server,
                provider_lookup=self.providers.get,
            ),
        )

        self._server = CastServer(
            certificates,
            host=config.bind_host,
            port=_CAST_PORT,
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

        enabled_providers = self.providers.all_providers()
        log.info("enabled providers: %s", _format_provider_summary(enabled_providers))

        if self._http.client.is_closed:
            self._http = ReceiverHTTPClient(data_dir=self.config.data_dir)

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

        await self._player_server.start()
        try:
            await self._server.start()
        except Exception:
            await self._player_server.stop()
            raise

        try:
            await self._eureka_server.start(certificates=self._certificates)
        except Exception:
            await self._server.stop()
            await self._player_server.stop()
            raise

        port = self._server.serving_port
        if port is None:
            await self._eureka_server.stop()
            await self._player_server.stop()
            await self._server.stop()
            msg = "server did not expose serving port"
            raise RuntimeError(msg)

        device_id = self.config.device_id
        if device_id is None:
            await self._eureka_server.stop()
            await self._player_server.stop()
            await self._server.stop()
            msg = "receiver device_id is not initialized"
            raise RuntimeError(msg)

        advertisement = CastAdvertisement(
            friendly_name=self.config.friendly_name,
            device_model=self.config.device_model,
            device_id=device_id,
            port=port,
            cert_digest=self._certificates.cert_digest_md5,
            app_ids=_collect_discovery_app_ids(enabled_providers),
        )
        try:
            await advertisement.start()
        except Exception:
            await self._eureka_server.stop()
            await self._player_server.stop()
            await self._server.stop()
            raise

        self._advertisement = advertisement
        self._started = True
        self._stop_event.clear()
        log.info(
            "cast receiver started (name=%s, host=%s, port=%d, service=%s, addresses=%s)",
            self.config.friendly_name,
            self.config.bind_host,
            port,
            advertisement.service_name,
            ",".join(advertisement.parsed_addresses),
        )

    async def stop(self) -> None:
        """Stop sessions, mDNS advertisement, and TLS server.

        Safe to call multiple times; subsequent calls are no-ops.
        """
        if not self._started:
            await self._eureka_server.stop()
            await self._player_server.stop()
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
            await self._eureka_server.stop()
            await self._player_server.stop()
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
        # Only remove subscriptions — do NOT stop orphaned sessions here.
        # Cast senders are expected to disconnect and reconnect (e.g. app
        # backgrounding, network transitions) while the session stays alive.
        # Sessions are only torn down by an explicit STOP request from a
        # sender or when the receiver shuts down.
        _ = self.device.remove_all_subscriptions(connection)


def _format_provider_summary(providers: list[Provider]) -> str:
    if not providers:
        return "none"

    entries: list[str] = []
    for provider in sorted(providers, key=lambda item: item.display_name().lower()):
        app_ids = ",".join(sorted(provider.app_ids()))
        entries.append(f"{provider.display_name()} (appIds={app_ids})")
    return "; ".join(entries)


def _collect_discovery_app_ids(providers: list[Provider]) -> frozenset[str]:
    app_ids: set[str] = set(_DISCOVERY_BASE_APP_IDS)
    for provider in providers:
        app_ids.update(provider.app_ids())
    return frozenset(app_ids)


__all__ = ["CastReceiver", "ReceiverConfig"]
