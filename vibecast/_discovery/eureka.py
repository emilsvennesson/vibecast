"""HTTP/HTTPS eureka_info endpoints used by sender discovery probes."""

from __future__ import annotations

import base64
import contextlib
import hashlib
import socket
import time
from dataclasses import asdict, dataclass
from typing import TYPE_CHECKING

from aiohttp import web
from cryptography.hazmat.primitives.serialization import Encoding, PublicFormat
from cryptography.x509 import load_der_x509_certificate
from pydantic import BaseModel, ConfigDict, Field

from vibecast._log import get_logger
from vibecast._security.tls import build_server_ssl_context, load_cert_chain

if TYPE_CHECKING:
    from collections.abc import Awaitable, Callable

    from vibecast._config import EurekaDeviceCapabilitiesConfig
    from vibecast._security.certificate import CertificateBundle

log = get_logger("eureka")


class _Model(BaseModel):
    model_config = ConfigDict(extra="forbid", populate_by_name=True)


class _Location(_Model):
    country_code: str = "US"
    latitude: float = 255.0
    longitude: float = 255.0


class _OptIn(_Model):
    crash: bool = False
    opencast: bool = False
    stats: bool = False


class _SetupStats(_Model):
    historically_succeeded: bool = True
    num_check_connectivity: int = 0
    num_connect_wifi: int = 0
    num_connected_wifi_not_saved: int = 0
    num_initial_eureka_info: int = 0
    num_obtain_ip: int = 0


class DeviceCapabilities(_Model):
    audio_hdr_supported: bool = False
    audio_surround_mode_supported: bool = False
    cast_connect_supported: bool = True
    cloud_groups_supported: bool = False
    cloudcast_supported: bool = True
    display_supported: bool = True
    fdr_supported: bool = False
    hdmi_prefer_50hz_supported: bool = False
    hdmi_prefer_high_fps_supported: bool = False
    hotspot_supported: bool = False
    https_setup_supported: bool = True
    keep_hotspot_until_connected_supported: bool = False
    multizone_supported: bool = True
    opencast_supported: bool = False
    reboot_supported: bool = False
    renaming_supported: bool = False
    set_group_audio_delay_supported: bool = False
    set_network_supported: bool = False
    setup_supported: bool = False
    stats_supported: bool = False
    system_sound_effects_supported: bool = False
    wifi_auto_save_supported: bool = False
    wifi_supported: bool = False


class EurekaDeviceInfo(_Model):
    capabilities: DeviceCapabilities
    cloud_device_id: str
    factory_country_code: str = ""
    hotspot_bssid: str = ""
    local_authorization_token_hash: str
    mac_address: str = "00:00:00:00:00:00"
    manufacturer: str = "Google Inc."
    model_name: str
    product_name: str
    public_key: str
    ssdp_udn: str
    uptime: float
    weave_device_id: str = ""


class EurekaMultizone(_Model):
    audio_output_delay: float = 0.0
    audio_output_delay_hdmi: float = 0.0
    audio_output_delay_oem: float = 0.0
    dynamic_groups: list[object] = Field(default_factory=list)
    groups: list[object] = Field(default_factory=list)
    max_static_groups: int = 100
    multichannel_status: int = 0


class EurekaInfo(_Model):
    bssid: str = ""
    build_version: str = "446070"
    cast_build_revision: str = "3.72.446070"
    connected: bool = True
    ethernet_connected: bool = True
    has_update: bool = False
    hotspot_bssid: str = ""
    ip_address: str
    locale: str = "en-US"
    location: _Location = Field(default_factory=_Location)
    mac_address: str = "00:00:00:00:00:00"
    name: str
    opt_in: _OptIn = Field(default_factory=_OptIn)
    public_key: str
    release_track: str = ""
    setup_state: int = 60
    setup_stats: _SetupStats = Field(default_factory=_SetupStats)
    ssdp_udn: str
    ssid: str = ""
    time_format: int = 1
    tos_accepted: bool = True
    uptime: float
    version: int = 12
    wpa_configured: bool = False
    wpa_state: int = 0
    device_info: EurekaDeviceInfo | None = None
    multizone: EurekaMultizone | None = None


@dataclass(slots=True, frozen=True)
class EurekaIdentity:
    friendly_name: str
    device_model: str
    ssdp_udn: str
    manufacturer: str = "Google Inc."
    locale: str = "en-US"
    country_code: str = "US"
    build_version: str = "446070"
    build_revision: str = "3.72.446070"
    capabilities: EurekaDeviceCapabilitiesConfig | None = None


class EurekaServer:
    """Expose Google Cast-style ``/setup/eureka_info`` endpoints."""

    __slots__ = (
        "_host",
        "_http_port",
        "_http_runner",
        "_http_serving_port",
        "_https_port",
        "_https_runner",
        "_https_serving_port",
        "_identity",
        "_ip_address",
        "_local_authorization_token_hash",
        "_public_key_b64",
        "_ssl_context",
        "_started_at",
    )

    def __init__(
        self,
        bundle: CertificateBundle,
        identity: EurekaIdentity,
        *,
        host: str,
        https_port: int,
        http_port: int,
    ) -> None:
        self._identity = identity
        self._host = host
        self._https_port = https_port
        self._http_port = http_port
        self._https_runner: web.AppRunner | None = None
        self._http_runner: web.AppRunner | None = None
        self._https_serving_port: int | None = None
        self._http_serving_port: int | None = None
        self._started_at = 0.0

        self._ip_address = _discover_primary_ipv4(host)
        self._public_key_b64 = _device_public_key_b64(bundle)
        self._ssl_context = build_server_ssl_context(bundle)
        self._local_authorization_token_hash = _token_hash(identity.ssdp_udn)
        log.debug(
            "eureka identity configured (name=%s model=%s ssdp_udn=%s bind_host=%s ip=%s public_key_b64_len=%d)",
            identity.friendly_name,
            identity.device_model,
            identity.ssdp_udn,
            host,
            self._ip_address,
            len(self._public_key_b64),
        )

    @property
    def https_serving_port(self) -> int | None:
        return self._https_serving_port

    @property
    def http_serving_port(self) -> int | None:
        return self._http_serving_port

    async def start(self, certificates: CertificateBundle) -> None:
        if self._https_runner is not None or self._http_runner is not None:
            log.debug("eureka server already started")
            return

        log.debug(
            "starting eureka server (host=%s, https_port=%d, http_port=%d)",
            self._host,
            self._https_port,
            self._http_port,
        )
        self._started_at = time.monotonic()
        self._public_key_b64 = _device_public_key_b64(certificates)
        self._ssl_context = build_server_ssl_context(certificates)
        ssl_context = self._ssl_context
        log.debug("eureka TLS context initialized")

        https_runner = web.AppRunner(_build_app(self._handle_eureka_info))
        await https_runner.setup()

        try:
            log.debug(
                "binding eureka HTTPS site (host=%s, port=%d)",
                self._host,
                self._https_port,
            )
            https_site = web.TCPSite(
                https_runner,
                self._host,
                self._https_port,
                ssl_context=ssl_context,
            )
            await https_site.start()
        except Exception:
            await https_runner.cleanup()
            raise

        http_runner = web.AppRunner(_build_app(self._handle_eureka_info))
        await http_runner.setup()

        try:
            log.debug(
                "binding eureka HTTP site (host=%s, port=%d)",
                self._host,
                self._http_port,
            )
            http_site = web.TCPSite(http_runner, self._host, self._http_port)
            await http_site.start()
        except Exception:
            await http_runner.cleanup()
            await https_runner.cleanup()
            raise

        self._https_serving_port = (
            _resolve_serving_port(https_runner) or self._https_port
        )
        self._http_serving_port = _resolve_serving_port(http_runner) or self._http_port
        self._https_runner = https_runner
        self._http_runner = http_runner
        log.info(
            "eureka server started (host=%s, https=%d, http=%d)",
            self._host,
            self._https_serving_port,
            self._http_serving_port,
        )

    def update_certificate(self, bundle: CertificateBundle) -> None:
        """Hot-reload certificate material for future HTTPS handshakes."""
        load_cert_chain(self._ssl_context, bundle)
        self._public_key_b64 = _device_public_key_b64(bundle)
        log.info(
            "eureka TLS certificate rotated (valid=%s -> %s)",
            bundle.not_valid_before.isoformat(),
            bundle.not_valid_after.isoformat(),
        )

    async def stop(self) -> None:
        http_runner = self._http_runner
        https_runner = self._https_runner
        self._http_runner = None
        self._https_runner = None

        if http_runner is None and https_runner is None:
            log.debug("eureka server already stopped")
            return

        with contextlib.suppress(Exception):
            if http_runner is not None:
                await http_runner.cleanup()
        with contextlib.suppress(Exception):
            if https_runner is not None:
                await https_runner.cleanup()

        self._http_serving_port = None
        self._https_serving_port = None
        self._started_at = 0.0
        log.info("eureka server stopped")

    async def _handle_eureka_info(self, request: web.Request) -> web.Response:
        raw_params = request.query.get("params")
        params = _parse_params(raw_params)
        payload = self._build_payload(params)
        log.debug(
            "served /setup/eureka_info (remote=%s scheme=%s params=%r keys=%d)",
            request.remote,
            request.scheme,
            raw_params,
            len(payload),
        )
        return web.json_response(payload)

    def _build_payload(self, params: tuple[str, ...] | None) -> dict[str, object]:
        uptime = max(time.monotonic() - self._started_at, 0.0)
        product_name = _product_name(self._identity.device_model)
        cloud_device_id = _cloud_device_id(self._identity.ssdp_udn)
        capabilities = (
            DeviceCapabilities(**asdict(self._identity.capabilities))
            if self._identity.capabilities is not None
            else DeviceCapabilities()
        )

        full_response = EurekaInfo(
            build_version=self._identity.build_version,
            cast_build_revision=self._identity.build_revision,
            ip_address=self._ip_address,
            locale=self._identity.locale,
            location=_Location(country_code=self._identity.country_code),
            name=self._identity.friendly_name,
            public_key=self._public_key_b64,
            ssdp_udn=self._identity.ssdp_udn,
            uptime=uptime,
            device_info=EurekaDeviceInfo(
                capabilities=capabilities,
                cloud_device_id=cloud_device_id,
                local_authorization_token_hash=self._local_authorization_token_hash,
                manufacturer=self._identity.manufacturer,
                model_name=self._identity.device_model,
                product_name=product_name,
                public_key=self._public_key_b64,
                ssdp_udn=self._identity.ssdp_udn,
                uptime=uptime,
            ),
            multizone=EurekaMultizone(),
        )

        encoded = full_response.model_dump(exclude_none=True)
        if params is None:
            # Match real devices: the non-filtered payload does not include these
            # optional blocks unless explicitly requested via ``?params=...``.
            _ = encoded.pop("device_info", None)
            _ = encoded.pop("multizone", None)
            return encoded

        return {key: encoded[key] for key in params if key in encoded}


def _build_app(
    handler: Callable[[web.Request], Awaitable[web.StreamResponse]],
) -> web.Application:
    app = web.Application()
    _ = app.router.add_get("/setup/eureka_info", handler)
    return app


def _resolve_serving_port(runner: web.AppRunner) -> int | None:
    addresses = runner.addresses
    if not addresses:
        return None

    host, port, *_ = addresses[0]
    _ = host
    return int(port)


def _discover_primary_ipv4(bind_host: str) -> str:
    if bind_host not in {"", "0.0.0.0", "::"}:
        log.debug("using explicit bind host as eureka IP: %s", bind_host)
        return bind_host

    try:
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
            sock.connect(("224.0.0.251", 5353))
            discovered = str(sock.getsockname()[0])
            log.debug(
                "discovered eureka primary IPv4 via multicast probe: %s", discovered
            )
            return discovered
    except OSError:
        log.debug(
            "failed to discover eureka primary IPv4 via multicast probe", exc_info=True
        )

    log.debug("falling back to loopback eureka IP: 127.0.0.1")
    return "127.0.0.1"


def _parse_params(raw: str | None) -> tuple[str, ...] | None:
    if raw is None:
        return None
    values = tuple(item.strip() for item in raw.split(",") if item.strip())
    return values if values else None


def _cloud_device_id(ssdp_udn: str) -> str:
    cleaned = ssdp_udn.replace("-", "").upper()
    if len(cleaned) == 32 and cleaned.isalnum():
        return cleaned
    return hashlib.md5(ssdp_udn.encode("utf-8")).hexdigest().upper()  # noqa: S324


def _product_name(model_name: str) -> str:
    normalized = "".join(ch.lower() for ch in model_name if ch.isalnum())
    return normalized or "chromecast"


def _token_hash(ssdp_udn: str) -> str:
    digest = hashlib.sha256(ssdp_udn.encode("utf-8")).digest()
    return base64.b64encode(digest).decode("ascii")


def _device_public_key_b64(bundle: CertificateBundle) -> str:
    cert = load_der_x509_certificate(bundle.device_cert_der)
    pub = cert.public_key().public_bytes(
        encoding=Encoding.DER,
        format=PublicFormat.SubjectPublicKeyInfo,
    )
    return base64.b64encode(pub).decode("ascii")


__all__ = [
    "DeviceCapabilities",
    "EurekaDeviceInfo",
    "EurekaIdentity",
    "EurekaInfo",
    "EurekaMultizone",
    "EurekaServer",
]
