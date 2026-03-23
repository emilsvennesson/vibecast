"""TOML-backed runtime configuration for vibecast."""

from __future__ import annotations

import json
import tomllib
from dataclasses import asdict, dataclass, field
from typing import TYPE_CHECKING, Any, cast

from vibecast._log import get_logger

if TYPE_CHECKING:
    from collections.abc import Mapping
    from pathlib import Path

log = get_logger("config")

_CONFIG_FILE_NAME = "config.toml"
_DEFAULT_CAST_USER_AGENT = (
    "Mozilla/5.0 (Linux; Android 11.0; Build/RQ1A.210105.003) "
    "AppleWebKit/537.36 (KHTML, like Gecko) Chrome/92.0.4515.0 "
    "Safari/537.36 CrKey/1.56.500000 DeviceType/AndroidTV"
)

_DEFAULT_CONFIG_CONTENT = """# vibecast configuration
# All values shown are defaults. Uncomment and modify as needed.

[device]
# Friendly name advertised over mDNS to Cast senders.
friendly_name = "vibecast"
# Device model string reported in mDNS and eureka discovery.
model = "Chromecast"
# Manufacturer name reported in eureka_info responses.
manufacturer = "Google Inc."
# Locale reported in eureka_info responses.
locale = "en-US"
# Country code reported in eureka_info location.
country_code = "US"
# Path to the receiver certificate bundle JSON.
# Relative paths are resolved from the data dir (usually ~/.vibecast).
# Can be overridden with the --certs CLI flag.
certs = "certs.json"
# Display resolution of the output device.
# Used by apps for adaptive stream selection.
display_width = 1920
display_height = 1080

# Eureka device capability flags reported to Cast senders via /setup/eureka_info.
# These mimic a real Chromecast. Override individual fields as needed.
[device.capabilities]
audio_hdr_supported = false
audio_surround_mode_supported = false
cast_connect_supported = true
cloud_groups_supported = false
cloudcast_supported = true
display_supported = true
fdr_supported = false
hdmi_prefer_50hz_supported = false
hdmi_prefer_high_fps_supported = false
hotspot_supported = false
https_setup_supported = true
keep_hotspot_until_connected_supported = false
multizone_supported = true
opencast_supported = false
reboot_supported = false
renaming_supported = false
set_group_audio_delay_supported = false
set_network_supported = false
setup_supported = false
stats_supported = false
system_sound_effects_supported = false
wifi_auto_save_supported = false
wifi_supported = false

[network]
# Host/interface to bind Cast, eureka, and player servers to.
bind_host = "0.0.0.0"
# Port for the player HTTP/WebSocket bridge server.
player_port = 8010
# Default HTTP client timeout in seconds (used for app API calls, CRL fetch).
http_timeout = 15.0
# Interval in seconds between certificate rotation checks.
cert_rotation_poll = 60.0

[volume]
# Initial volume level (0.0 to 1.0).
level = 1.0
# Whether the receiver starts muted.
muted = false
# Volume adjustment step size (used by sender UI).
step_interval = 0.05

[cast]
# Cast firmware build version and revision reported in eureka_info.
build_version = "446070"
build_revision = "3.72.446070"
# User-Agent string sent by apps to streaming APIs.
# Must mimic a real Chromecast for streaming APIs to accept requests.
user_agent = "Mozilla/5.0 (Linux; Android 11.0; Build/RQ1A.210105.003) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/92.0.4515.0 Safari/537.36 CrKey/1.56.500000 DeviceType/AndroidTV"

# HTTP header capabilities sent by apps to streaming service APIs.
# Separate from [device.capabilities] which is for eureka/mDNS discovery.
[cast.device_capabilities]
display_supported = true
hi_res_audio_supported = false
remote_control_input_supported = true
touch_input_supported = false

[apps.primevideo]
# Amazon marketplace ID. Determines regional API endpoint behavior.
marketplace_id = "A3K6Y4MI8GDYMT"
# Locale for Prime Video API requests.
locale = "en_US"
# Amazon auth API base URL. Change for different regions.
auth_base_url = "https://api.amazon.co.uk"
# Device capability parameters sent to Amazon playback APIs.
hdcp_level = "1.4"
max_video_resolution = "1080p"
supported_codecs = ["H265", "H264"]
dynamic_range_formats = ["None"]
supported_frame_rates = ["Standard", "High"]
supported_subtitle_formats = ["TTMLv2", "DFXP"]
"""


@dataclass(frozen=True, slots=True)
class EurekaDeviceCapabilitiesConfig:
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


@dataclass(frozen=True, slots=True)
class DeviceConfig:
    friendly_name: str = "vibecast"
    model: str = "Chromecast"
    manufacturer: str = "Google Inc."
    locale: str = "en-US"
    country_code: str = "US"
    certs: str = "certs.json"
    display_width: int = 1920
    display_height: int = 1080
    capabilities: EurekaDeviceCapabilitiesConfig = field(
        default_factory=EurekaDeviceCapabilitiesConfig
    )


@dataclass(frozen=True, slots=True)
class NetworkConfig:
    bind_host: str = "0.0.0.0"
    player_port: int = 8010
    http_timeout: float = 15.0
    cert_rotation_poll: float = 60.0


@dataclass(frozen=True, slots=True)
class VolumeConfig:
    level: float = 1.0
    muted: bool = False
    step_interval: float = 0.05


@dataclass(frozen=True, slots=True)
class CastDeviceCapabilitiesConfig:
    display_supported: bool = True
    hi_res_audio_supported: bool = False
    remote_control_input_supported: bool = True
    touch_input_supported: bool = False


@dataclass(frozen=True, slots=True)
class CastConfig:
    build_version: str = "446070"
    build_revision: str = "3.72.446070"
    user_agent: str = _DEFAULT_CAST_USER_AGENT
    device_capabilities: CastDeviceCapabilitiesConfig = field(
        default_factory=CastDeviceCapabilitiesConfig
    )


@dataclass(frozen=True, slots=True)
class VibecastConfig:
    device: DeviceConfig = field(default_factory=DeviceConfig)
    network: NetworkConfig = field(default_factory=NetworkConfig)
    volume: VolumeConfig = field(default_factory=VolumeConfig)
    cast: CastConfig = field(default_factory=CastConfig)
    apps: dict[str, dict[str, Any]] = field(default_factory=dict)


def load_config(data_dir: Path) -> VibecastConfig:
    """Load receiver config from ``{data_dir}/config.toml``.

    If the file does not exist, a commented default is generated first.
    """

    config_path = data_dir / _CONFIG_FILE_NAME
    if not config_path.exists():
        _write_default_config(config_path)
        log.info("generated default config at %s", config_path)

    try:
        with config_path.open("rb") as handle:
            raw_data = tomllib.load(handle)
    except tomllib.TOMLDecodeError as exc:
        msg = f"invalid TOML in {config_path}: {exc}"
        raise ValueError(msg) from exc

    return _parse_config(raw=raw_data, config_path=config_path)


def cast_device_capabilities_header(caps: CastDeviceCapabilitiesConfig) -> str:
    """Serialize CAST-DEVICE-CAPABILITIES JSON header payload."""

    return json.dumps(asdict(caps), separators=(",", ":"))


def _write_default_config(path: Path) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    _ = path.write_text(_DEFAULT_CONFIG_CONTENT, encoding="utf-8")


def _parse_config(*, raw: Mapping[str, Any], config_path: Path) -> VibecastConfig:
    _validate_unknown_keys(
        raw,
        {"device", "network", "volume", "cast", "apps"},
        path="config",
        config_path=config_path,
    )

    device_section = _table(raw, "device", path="config", config_path=config_path)
    network_section = _table(raw, "network", path="config", config_path=config_path)
    volume_section = _table(raw, "volume", path="config", config_path=config_path)
    cast_section = _table(raw, "cast", path="config", config_path=config_path)
    apps_section = _table(raw, "apps", path="config", config_path=config_path)

    device = _parse_device_config(device_section, config_path=config_path)
    network = _parse_network_config(network_section, config_path=config_path)
    volume = _parse_volume_config(volume_section, config_path=config_path)
    cast_config = _parse_cast_config(cast_section, config_path=config_path)
    apps = _parse_app_tables(apps_section, config_path=config_path)

    return VibecastConfig(
        device=device,
        network=network,
        volume=volume,
        cast=cast_config,
        apps=apps,
    )


def _parse_device_config(
    values: Mapping[str, Any],
    *,
    config_path: Path,
) -> DeviceConfig:
    defaults = DeviceConfig()
    _validate_unknown_keys(
        values,
        {
            "friendly_name",
            "model",
            "manufacturer",
            "locale",
            "country_code",
            "certs",
            "display_width",
            "display_height",
            "capabilities",
        },
        path="device",
        config_path=config_path,
    )
    capabilities_values = _table(
        values,
        "capabilities",
        path="device",
        config_path=config_path,
    )
    capabilities = _parse_eureka_device_capabilities(
        capabilities_values,
        config_path=config_path,
    )

    certs = _string(
        values,
        "certs",
        defaults.certs,
        path="device",
        config_path=config_path,
    )

    return DeviceConfig(
        friendly_name=_string(
            values,
            "friendly_name",
            defaults.friendly_name,
            path="device",
            config_path=config_path,
        ),
        model=_string(
            values,
            "model",
            defaults.model,
            path="device",
            config_path=config_path,
        ),
        manufacturer=_string(
            values,
            "manufacturer",
            defaults.manufacturer,
            path="device",
            config_path=config_path,
        ),
        locale=_string(
            values,
            "locale",
            defaults.locale,
            path="device",
            config_path=config_path,
        ),
        country_code=_string(
            values,
            "country_code",
            defaults.country_code,
            path="device",
            config_path=config_path,
        ),
        certs=certs,
        display_width=_integer(
            values,
            "display_width",
            defaults.display_width,
            path="device",
            config_path=config_path,
        ),
        display_height=_integer(
            values,
            "display_height",
            defaults.display_height,
            path="device",
            config_path=config_path,
        ),
        capabilities=capabilities,
    )


def _parse_eureka_device_capabilities(
    values: Mapping[str, Any],
    *,
    config_path: Path,
) -> EurekaDeviceCapabilitiesConfig:
    defaults = EurekaDeviceCapabilitiesConfig()
    _validate_unknown_keys(
        values,
        {
            "audio_hdr_supported",
            "audio_surround_mode_supported",
            "cast_connect_supported",
            "cloud_groups_supported",
            "cloudcast_supported",
            "display_supported",
            "fdr_supported",
            "hdmi_prefer_50hz_supported",
            "hdmi_prefer_high_fps_supported",
            "hotspot_supported",
            "https_setup_supported",
            "keep_hotspot_until_connected_supported",
            "multizone_supported",
            "opencast_supported",
            "reboot_supported",
            "renaming_supported",
            "set_group_audio_delay_supported",
            "set_network_supported",
            "setup_supported",
            "stats_supported",
            "system_sound_effects_supported",
            "wifi_auto_save_supported",
            "wifi_supported",
        },
        path="device.capabilities",
        config_path=config_path,
    )
    return EurekaDeviceCapabilitiesConfig(
        audio_hdr_supported=_boolean(
            values,
            "audio_hdr_supported",
            defaults.audio_hdr_supported,
            path="device.capabilities",
            config_path=config_path,
        ),
        audio_surround_mode_supported=_boolean(
            values,
            "audio_surround_mode_supported",
            defaults.audio_surround_mode_supported,
            path="device.capabilities",
            config_path=config_path,
        ),
        cast_connect_supported=_boolean(
            values,
            "cast_connect_supported",
            defaults.cast_connect_supported,
            path="device.capabilities",
            config_path=config_path,
        ),
        cloud_groups_supported=_boolean(
            values,
            "cloud_groups_supported",
            defaults.cloud_groups_supported,
            path="device.capabilities",
            config_path=config_path,
        ),
        cloudcast_supported=_boolean(
            values,
            "cloudcast_supported",
            defaults.cloudcast_supported,
            path="device.capabilities",
            config_path=config_path,
        ),
        display_supported=_boolean(
            values,
            "display_supported",
            defaults.display_supported,
            path="device.capabilities",
            config_path=config_path,
        ),
        fdr_supported=_boolean(
            values,
            "fdr_supported",
            defaults.fdr_supported,
            path="device.capabilities",
            config_path=config_path,
        ),
        hdmi_prefer_50hz_supported=_boolean(
            values,
            "hdmi_prefer_50hz_supported",
            defaults.hdmi_prefer_50hz_supported,
            path="device.capabilities",
            config_path=config_path,
        ),
        hdmi_prefer_high_fps_supported=_boolean(
            values,
            "hdmi_prefer_high_fps_supported",
            defaults.hdmi_prefer_high_fps_supported,
            path="device.capabilities",
            config_path=config_path,
        ),
        hotspot_supported=_boolean(
            values,
            "hotspot_supported",
            defaults.hotspot_supported,
            path="device.capabilities",
            config_path=config_path,
        ),
        https_setup_supported=_boolean(
            values,
            "https_setup_supported",
            defaults.https_setup_supported,
            path="device.capabilities",
            config_path=config_path,
        ),
        keep_hotspot_until_connected_supported=_boolean(
            values,
            "keep_hotspot_until_connected_supported",
            defaults.keep_hotspot_until_connected_supported,
            path="device.capabilities",
            config_path=config_path,
        ),
        multizone_supported=_boolean(
            values,
            "multizone_supported",
            defaults.multizone_supported,
            path="device.capabilities",
            config_path=config_path,
        ),
        opencast_supported=_boolean(
            values,
            "opencast_supported",
            defaults.opencast_supported,
            path="device.capabilities",
            config_path=config_path,
        ),
        reboot_supported=_boolean(
            values,
            "reboot_supported",
            defaults.reboot_supported,
            path="device.capabilities",
            config_path=config_path,
        ),
        renaming_supported=_boolean(
            values,
            "renaming_supported",
            defaults.renaming_supported,
            path="device.capabilities",
            config_path=config_path,
        ),
        set_group_audio_delay_supported=_boolean(
            values,
            "set_group_audio_delay_supported",
            defaults.set_group_audio_delay_supported,
            path="device.capabilities",
            config_path=config_path,
        ),
        set_network_supported=_boolean(
            values,
            "set_network_supported",
            defaults.set_network_supported,
            path="device.capabilities",
            config_path=config_path,
        ),
        setup_supported=_boolean(
            values,
            "setup_supported",
            defaults.setup_supported,
            path="device.capabilities",
            config_path=config_path,
        ),
        stats_supported=_boolean(
            values,
            "stats_supported",
            defaults.stats_supported,
            path="device.capabilities",
            config_path=config_path,
        ),
        system_sound_effects_supported=_boolean(
            values,
            "system_sound_effects_supported",
            defaults.system_sound_effects_supported,
            path="device.capabilities",
            config_path=config_path,
        ),
        wifi_auto_save_supported=_boolean(
            values,
            "wifi_auto_save_supported",
            defaults.wifi_auto_save_supported,
            path="device.capabilities",
            config_path=config_path,
        ),
        wifi_supported=_boolean(
            values,
            "wifi_supported",
            defaults.wifi_supported,
            path="device.capabilities",
            config_path=config_path,
        ),
    )


def _parse_network_config(
    values: Mapping[str, Any],
    *,
    config_path: Path,
) -> NetworkConfig:
    defaults = NetworkConfig()
    _validate_unknown_keys(
        values,
        {"bind_host", "player_port", "http_timeout", "cert_rotation_poll"},
        path="network",
        config_path=config_path,
    )
    return NetworkConfig(
        bind_host=_string(
            values,
            "bind_host",
            defaults.bind_host,
            path="network",
            config_path=config_path,
        ),
        player_port=_integer(
            values,
            "player_port",
            defaults.player_port,
            path="network",
            config_path=config_path,
        ),
        http_timeout=_float(
            values,
            "http_timeout",
            defaults.http_timeout,
            path="network",
            config_path=config_path,
        ),
        cert_rotation_poll=_float(
            values,
            "cert_rotation_poll",
            defaults.cert_rotation_poll,
            path="network",
            config_path=config_path,
        ),
    )


def _parse_volume_config(
    values: Mapping[str, Any],
    *,
    config_path: Path,
) -> VolumeConfig:
    defaults = VolumeConfig()
    _validate_unknown_keys(
        values,
        {"level", "muted", "step_interval"},
        path="volume",
        config_path=config_path,
    )
    return VolumeConfig(
        level=_float(
            values,
            "level",
            defaults.level,
            path="volume",
            config_path=config_path,
        ),
        muted=_boolean(
            values,
            "muted",
            defaults.muted,
            path="volume",
            config_path=config_path,
        ),
        step_interval=_float(
            values,
            "step_interval",
            defaults.step_interval,
            path="volume",
            config_path=config_path,
        ),
    )


def _parse_cast_config(
    values: Mapping[str, Any],
    *,
    config_path: Path,
) -> CastConfig:
    defaults = CastConfig()
    _validate_unknown_keys(
        values,
        {"build_version", "build_revision", "user_agent", "device_capabilities"},
        path="cast",
        config_path=config_path,
    )
    capabilities_values = _table(
        values,
        "device_capabilities",
        path="cast",
        config_path=config_path,
    )
    capabilities = _parse_cast_device_capabilities(
        capabilities_values,
        config_path=config_path,
    )

    return CastConfig(
        build_version=_string(
            values,
            "build_version",
            defaults.build_version,
            path="cast",
            config_path=config_path,
        ),
        build_revision=_string(
            values,
            "build_revision",
            defaults.build_revision,
            path="cast",
            config_path=config_path,
        ),
        user_agent=_string(
            values,
            "user_agent",
            defaults.user_agent,
            path="cast",
            config_path=config_path,
        ),
        device_capabilities=capabilities,
    )


def _parse_cast_device_capabilities(
    values: Mapping[str, Any],
    *,
    config_path: Path,
) -> CastDeviceCapabilitiesConfig:
    defaults = CastDeviceCapabilitiesConfig()
    _validate_unknown_keys(
        values,
        {
            "display_supported",
            "hi_res_audio_supported",
            "remote_control_input_supported",
            "touch_input_supported",
        },
        path="cast.device_capabilities",
        config_path=config_path,
    )

    return CastDeviceCapabilitiesConfig(
        display_supported=_boolean(
            values,
            "display_supported",
            defaults.display_supported,
            path="cast.device_capabilities",
            config_path=config_path,
        ),
        hi_res_audio_supported=_boolean(
            values,
            "hi_res_audio_supported",
            defaults.hi_res_audio_supported,
            path="cast.device_capabilities",
            config_path=config_path,
        ),
        remote_control_input_supported=_boolean(
            values,
            "remote_control_input_supported",
            defaults.remote_control_input_supported,
            path="cast.device_capabilities",
            config_path=config_path,
        ),
        touch_input_supported=_boolean(
            values,
            "touch_input_supported",
            defaults.touch_input_supported,
            path="cast.device_capabilities",
            config_path=config_path,
        ),
    )


def _parse_app_tables(
    values: Mapping[str, Any],
    *,
    config_path: Path,
) -> dict[str, dict[str, Any]]:
    apps: dict[str, dict[str, Any]] = {}
    for key, value in values.items():
        if not isinstance(value, dict):
            location = f"apps.{key}"
            msg = f"{config_path}: {location} must be a table"
            raise TypeError(msg)
        apps[key] = dict(cast("dict[str, Any]", value))
    return apps


def _table(
    values: Mapping[str, Any],
    key: str,
    *,
    path: str,
    config_path: Path,
) -> Mapping[str, Any]:
    raw = values.get(key)
    if raw is None:
        return {}
    if not isinstance(raw, dict):
        location = f"{path}.{key}" if path else key
        msg = f"{config_path}: {location} must be a table"
        raise TypeError(msg)
    return cast("Mapping[str, Any]", raw)


def _validate_unknown_keys(
    values: Mapping[str, Any],
    known_keys: set[str],
    *,
    path: str,
    config_path: Path,
) -> None:
    unknown = sorted(key for key in values if key not in known_keys)
    if not unknown:
        return
    msg = f"{config_path}: unknown keys in [{path}]: {', '.join(unknown)}"
    raise ValueError(msg)


def _string(
    values: Mapping[str, Any],
    key: str,
    default: str,
    *,
    path: str,
    config_path: Path,
) -> str:
    value = values.get(key, default)
    if isinstance(value, str):
        return value
    _raise_type_error(
        config_path=config_path,
        path=path,
        key=key,
        expected="string",
        actual=value,
    )
    msg = "unreachable"
    raise AssertionError(msg)


def _integer(
    values: Mapping[str, Any],
    key: str,
    default: int,
    *,
    path: str,
    config_path: Path,
) -> int:
    value = values.get(key, default)
    if isinstance(value, bool) or not isinstance(value, int):
        _raise_type_error(
            config_path=config_path,
            path=path,
            key=key,
            expected="integer",
            actual=value,
        )
    return value


def _float(
    values: Mapping[str, Any],
    key: str,
    default: float,
    *,
    path: str,
    config_path: Path,
) -> float:
    value = values.get(key, default)
    if isinstance(value, bool) or not isinstance(value, int | float):
        _raise_type_error(
            config_path=config_path,
            path=path,
            key=key,
            expected="float",
            actual=value,
        )
    return float(value)


def _boolean(
    values: Mapping[str, Any],
    key: str,
    default: bool,
    *,
    path: str,
    config_path: Path,
) -> bool:
    value = values.get(key, default)
    if isinstance(value, bool):
        return value
    _raise_type_error(
        config_path=config_path,
        path=path,
        key=key,
        expected="boolean",
        actual=value,
    )
    msg = "unreachable"
    raise AssertionError(msg)


def _raise_type_error(
    *,
    config_path: Path,
    path: str,
    key: str,
    expected: str,
    actual: object,
) -> None:
    location = f"{path}.{key}" if path else key
    msg = (
        f"{config_path}: {location} must be a {expected}, "
        f"got {type(actual).__name__}: {actual!r}"
    )
    raise TypeError(msg)


__all__ = [
    "CastConfig",
    "CastDeviceCapabilitiesConfig",
    "DeviceConfig",
    "EurekaDeviceCapabilitiesConfig",
    "NetworkConfig",
    "VibecastConfig",
    "VolumeConfig",
    "cast_device_capabilities_header",
    "load_config",
]
