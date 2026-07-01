//! Typed TOML configuration.
//!
//! Redesign of the Python `_config` loader: serde derives replace ~700 lines of
//! hand-written per-field validation. `#[serde(default)]` gives per-field
//! fallbacks (a missing file or key uses the Chromecast-like defaults) and
//! `deny_unknown_fields` gives the same unknown-key rejection with clear errors.
//! Config lives in the platform binary only; the portable core never sees it.

use std::collections::HashMap;
use std::path::Path;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use vibecast_discovery::DeviceCapabilities;

const CONFIG_FILE: &str = "config.toml";

const DEFAULT_USER_AGENT: &str = "Mozilla/5.0 (Linux; Android 11.0; Build/RQ1A.210105.003) \
AppleWebKit/537.36 (KHTML, like Gecko) Chrome/92.0.4515.0 Safari/537.36 \
CrKey/1.56.500000 DeviceType/AndroidTV";

/// Top-level receiver configuration.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Device identity + display + eureka capabilities.
    pub device: DeviceConfig,
    /// Network binding, ports, and timeouts.
    pub network: NetworkConfig,
    /// Initial volume.
    pub volume: VolumeConfig,
    /// Cast firmware identity + streaming-API capabilities.
    pub cast: CastConfig,
    /// Per-app config tables (`[apps.<key>]`), consumed by `AppProvider::configure`.
    pub apps: HashMap<String, toml::Table>,
}

/// `[device]` section.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DeviceConfig {
    /// Friendly name advertised to senders.
    pub friendly_name: String,
    /// Device model string.
    pub model: String,
    /// Manufacturer reported in eureka_info.
    pub manufacturer: String,
    /// Locale reported in eureka_info.
    pub locale: String,
    /// Country code reported in eureka_info.
    pub country_code: String,
    /// Certificate bundle path (relative paths resolve from the data dir).
    pub certs: String,
    /// Output display width in pixels.
    pub display_width: u32,
    /// Output display height in pixels.
    pub display_height: u32,
    /// Eureka device-capability flags (reused from the discovery crate).
    pub capabilities: DeviceCapabilities,
}

impl Default for DeviceConfig {
    fn default() -> Self {
        Self {
            friendly_name: "vibecast".into(),
            model: "Chromecast".into(),
            manufacturer: "Google Inc.".into(),
            locale: "en-US".into(),
            country_code: "US".into(),
            certs: "certs.json".into(),
            display_width: 1920,
            display_height: 1080,
            capabilities: DeviceCapabilities::default(),
        }
    }
}

/// `[network]` section.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NetworkConfig {
    /// Host/interface to bind all listeners to.
    pub bind_host: String,
    /// Player bridge port.
    pub player_port: u16,
    /// HTTP client timeout (seconds).
    pub http_timeout: f64,
    /// Certificate-rotation poll interval (seconds).
    pub cert_rotation_poll: f64,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            bind_host: "0.0.0.0".into(),
            player_port: 8010,
            http_timeout: 15.0,
            cert_rotation_poll: 60.0,
        }
    }
}

/// `[volume]` section.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct VolumeConfig {
    /// Initial level in `[0.0, 1.0]`.
    pub level: f64,
    /// Whether the receiver starts muted.
    pub muted: bool,
    /// Volume step granularity.
    pub step_interval: f64,
}

impl Default for VolumeConfig {
    fn default() -> Self {
        Self {
            level: 1.0,
            muted: false,
            step_interval: 0.05,
        }
    }
}

/// `[cast]` section.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CastConfig {
    /// Firmware build version reported in eureka_info.
    pub build_version: String,
    /// Firmware build revision reported in eureka_info.
    pub build_revision: String,
    /// User-Agent apps send to streaming APIs.
    pub user_agent: String,
    /// Capabilities sent in the `CAST-DEVICE-CAPABILITIES` HTTP header.
    pub device_capabilities: CastDeviceCapabilities,
}

impl Default for CastConfig {
    fn default() -> Self {
        Self {
            build_version: "446070".into(),
            build_revision: "3.72.446070".into(),
            user_agent: DEFAULT_USER_AGENT.into(),
            device_capabilities: CastDeviceCapabilities::default(),
        }
    }
}

/// `[cast.device_capabilities]` — the streaming-API capability header.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CastDeviceCapabilities {
    /// Display output supported.
    pub display_supported: bool,
    /// Hi-res audio supported.
    pub hi_res_audio_supported: bool,
    /// Remote-control input supported.
    pub remote_control_input_supported: bool,
    /// Touch input supported.
    pub touch_input_supported: bool,
}

impl Default for CastDeviceCapabilities {
    fn default() -> Self {
        Self {
            display_supported: true,
            hi_res_audio_supported: false,
            remote_control_input_supported: true,
            touch_input_supported: false,
        }
    }
}

impl CastDeviceCapabilities {
    /// The compact JSON value for the `CAST-DEVICE-CAPABILITIES` header.
    #[must_use]
    pub fn header_value(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

impl Config {
    /// Load `{data_dir}/config.toml`. A missing file yields all defaults.
    pub fn load(data_dir: &Path) -> anyhow::Result<Self> {
        let path = data_dir.join(CONFIG_FILE);
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error).with_context(|| format!("reading {}", path.display())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_is_all_defaults() {
        let config: Config = toml::from_str("").unwrap();
        assert_eq!(config.device.friendly_name, "vibecast");
        assert_eq!(config.network.player_port, 8010);
        assert_eq!(config.volume.level, 1.0);
        assert!(config.cast.user_agent.contains("CrKey"));
        assert!(config.device.capabilities.cast_connect_supported);
    }

    #[test]
    fn partial_config_overrides_only_named_fields() {
        let config: Config = toml::from_str(
            r#"
            [device]
            friendly_name = "Living Room"
            [device.capabilities]
            multizone_supported = false
            [network]
            player_port = 9010
            [apps.primevideo]
            marketplace_id = "X"
            "#,
        )
        .unwrap();
        assert_eq!(config.device.friendly_name, "Living Room");
        assert_eq!(config.device.model, "Chromecast"); // default preserved
        assert!(!config.device.capabilities.multizone_supported); // overridden
        assert!(config.device.capabilities.cast_connect_supported); // default preserved
        assert_eq!(config.network.player_port, 9010);
        assert!(config.apps.contains_key("primevideo"));
    }

    #[test]
    fn unknown_top_level_key_is_rejected() {
        assert!(toml::from_str::<Config>("[bogus]\nx = 1\n").is_err());
    }

    #[test]
    fn cast_capabilities_header_is_compact_json() {
        let header = CastDeviceCapabilities::default().header_value();
        assert_eq!(
            header,
            r#"{"display_supported":true,"hi_res_audio_supported":false,"remote_control_input_supported":true,"touch_input_supported":false}"#
        );
    }
}
