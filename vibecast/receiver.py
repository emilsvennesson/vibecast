"""Public Cast receiver orchestrator."""

from __future__ import annotations

import asyncio
import contextlib
from pathlib import Path
from typing import TYPE_CHECKING, Final
from uuid import uuid4

from vibecast._auth import fetch_crl
from vibecast._certificate import CertificateBundle, CertificateStore
from vibecast._config import VibecastConfig, cast_device_capabilities_header
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
    from vibecast._connection import Connection
    from vibecast._proto.cast_channel_pb2 import CastMessage

log = get_logger("receiver")

_CAST_PORT: Final[int] = 8009
_EUREKA_HTTPS_PORT: Final[int] = 8443
_EUREKA_HTTP_PORT: Final[int] = 8008
_DISCOVERY_BASE_APP_IDS: Final[frozenset[str]] = frozenset({"CC1AD845", "0F5096E8"})
_DEFAULT_DATA_DIR: Final[Path] = Path.home() / ".vibecast"


class CastReceiver:
    """High-level receiver API wiring server, routing, and mDNS."""

    __slots__ = (
        "_advertisement",
        "_certificate_store",
        "_cert_rotation_task",
        "_certificates",
        "_data_dir",
        "_device_id",
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
        config: VibecastConfig,
        certificates: CertificateStore | CertificateBundle,
        providers: list[Provider] | None = None,
        *,
        device_id: str | None = None,
        data_dir: Path = _DEFAULT_DATA_DIR,
    ) -> None:
        resolved_device_id = device_id or str(uuid4())

        certificate_store = (
            certificates
            if isinstance(certificates, CertificateStore)
            else CertificateStore.from_bundle(certificates)
        )
        active_bundle = certificate_store.active_bundle
        cast_capabilities = cast_device_capabilities_header(
            config.cast.device_capabilities
        )

        self.config = config
        self._data_dir = data_dir
        self._device_id = resolved_device_id
        self._certificate_store = certificate_store
        self._certificates = active_bundle
        self._http = ReceiverHTTPClient(
            data_dir=data_dir,
            timeout_seconds=config.network.http_timeout,
        )

        provider_instances = discover_providers() if providers is None else providers
        for provider in provider_instances:
            provider_config = config.providers.get(provider.provider_key(), {})
            provider.configure(provider_config)
        self.providers = ProviderRegistry(provider_instances)

        self.device = Device(
            DeviceIdentity(
                friendly_name=config.device.friendly_name,
                device_model=config.device.model,
                device_id=resolved_device_id,
            ),
            get_http_client=lambda: self._http.client,
            data_dir=data_dir,
            volume_level=config.volume.level,
            volume_muted=config.volume.muted,
            volume_step_interval=config.volume.step_interval,
            receiver_user_agent=config.cast.user_agent,
            receiver_cast_device_capabilities=cast_capabilities,
            receiver_display_width=config.device.display_width,
            receiver_display_height=config.device.display_height,
        )

        self._player_server = PlayerServer(
            host=config.network.bind_host,
            port=config.network.player_port,
        )

        self._eureka_server = EurekaServer(
            active_bundle,
            EurekaIdentity(
                friendly_name=config.device.friendly_name,
                device_model=config.device.model,
                ssdp_udn=resolved_device_id,
                manufacturer=config.device.manufacturer,
                locale=config.device.locale,
                country_code=config.device.country_code,
                build_version=config.cast.build_version,
                build_revision=config.cast.build_revision,
                capabilities=config.device.capabilities,
            ),
            host=config.network.bind_host,
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
            active_bundle,
            host=config.network.bind_host,
            port=_CAST_PORT,
            on_message=self._on_message,
            on_disconnect=self._on_disconnect,
        )
        self._advertisement: CastAdvertisement | None = None
        self._cert_rotation_task: asyncio.Task[None] | None = None
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
            self._http = ReceiverHTTPClient(
                data_dir=self._data_dir,
                timeout_seconds=self.config.network.http_timeout,
            )

        rotated = self._certificate_store.rotate_if_needed()
        if rotated is not None:
            self._server.update_certificate(rotated)
            self._eureka_server.update_certificate(rotated)
            self._certificates = rotated

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

        device_id = self._device_id

        advertisement = CastAdvertisement(
            friendly_name=self.config.device.friendly_name,
            device_model=self.config.device.model,
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
        self._cert_rotation_task = asyncio.create_task(self._run_certificate_rotation())
        self._started = True
        self._stop_event.clear()
        log.info(
            "cast receiver started (name=%s, host=%s, port=%d, service=%s, addresses=%s)",
            self.config.device.friendly_name,
            self.config.network.bind_host,
            port,
            advertisement.service_name,
            ",".join(advertisement.parsed_addresses),
        )

    async def stop(self) -> None:
        """Stop sessions, mDNS advertisement, and TLS server.

        Safe to call multiple times; subsequent calls are no-ops.
        """
        if not self._started:
            await self._stop_certificate_rotation()
            await self._eureka_server.stop()
            await self._player_server.stop()
            await self._http.close()
            return
        self._stop_event.set()
        await self._stop_certificate_rotation()

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

    async def _run_certificate_rotation(self) -> None:
        while True:
            await asyncio.sleep(self.config.network.cert_rotation_poll)
            try:
                rotated = self._certificate_store.rotate_if_needed()
            except asyncio.CancelledError:
                raise
            except Exception:
                log.exception("certificate rotation failed")
                continue

            if rotated is None:
                continue

            try:
                await self._apply_certificate_rotation(rotated)
            except asyncio.CancelledError:
                raise
            except Exception:
                log.exception("failed to apply rotated certificate")

    async def _apply_certificate_rotation(self, bundle: CertificateBundle) -> None:
        self._server.update_certificate(bundle)
        self._eureka_server.update_certificate(bundle)

        if self._advertisement is not None:
            await self._advertisement.update_cert_digest(bundle.cert_digest_md5)

        self._certificates = bundle
        log.info(
            "active certificate rotated (valid=%s -> %s)",
            bundle.not_valid_before.isoformat(),
            bundle.not_valid_after.isoformat(),
        )

    async def _stop_certificate_rotation(self) -> None:
        task = self._cert_rotation_task
        self._cert_rotation_task = None
        if task is None:
            return

        _ = task.cancel()
        with contextlib.suppress(asyncio.CancelledError):
            await task


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


__all__ = ["CastReceiver"]
