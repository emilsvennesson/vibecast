"""Tests for TOML configuration loading and defaults."""

from __future__ import annotations

from typing import TYPE_CHECKING

import pytest

from vibecast._config import (
    CastDeviceCapabilitiesConfig,
    cast_device_capabilities_header,
    load_config,
)

if TYPE_CHECKING:
    from pathlib import Path


def test_load_config_generates_default_file(tmp_path: Path) -> None:
    config = load_config(tmp_path)

    config_path = tmp_path / "config.toml"
    assert config_path.exists()
    content = config_path.read_text(encoding="utf-8")
    assert "# vibecast configuration" in content
    assert "[device]" in content
    assert "[providers.primevideo]" in content

    assert config.device.friendly_name == "vibecast"
    assert config.device.certs == "certs.json"
    assert config.network.player_port == 8010
    assert config.cast.build_revision == "3.72.446070"


def test_load_config_merges_partial_values(tmp_path: Path) -> None:
    config_path = tmp_path / "config.toml"
    _ = config_path.write_text(
        """
[device]
friendly_name = "Kitchen"
certs = "/tmp/certs.json"

[network]
player_port = 19010

[providers.viaplay]
country_code = "no"
""".strip(),
        encoding="utf-8",
    )

    config = load_config(tmp_path)

    assert config.device.friendly_name == "Kitchen"
    assert config.device.certs == "/tmp/certs.json"
    assert config.device.model == "Chromecast"
    assert config.network.player_port == 19010
    assert config.network.bind_host == "0.0.0.0"
    assert config.providers["viaplay"]["country_code"] == "no"


def test_load_config_validates_field_types(tmp_path: Path) -> None:
    config_path = tmp_path / "config.toml"
    _ = config_path.write_text(
        """
[network]
player_port = "8010"
""".strip(),
        encoding="utf-8",
    )

    with pytest.raises(TypeError) as exc_info:
        _ = load_config(tmp_path)

    message = str(exc_info.value)
    assert "network.player_port" in message
    assert "integer" in message


def test_cast_device_capabilities_header_serialization() -> None:
    header = cast_device_capabilities_header(
        CastDeviceCapabilitiesConfig(
            display_supported=False,
            hi_res_audio_supported=True,
            remote_control_input_supported=False,
            touch_input_supported=True,
        )
    )

    assert (
        header
        == '{"display_supported":false,"hi_res_audio_supported":true,"remote_control_input_supported":false,"touch_input_supported":true}'
    )
