//! App-provider configuration helpers.
//!
//! The SDK exposes configuration as a format-neutral JSON value so app crates do
//! not depend on the host's config file format (currently TOML in the CLI).

use serde::de::DeserializeOwned;
use serde_json::Value;

/// Format-neutral configuration for one app provider.
#[derive(Debug, Clone)]
pub struct AppConfig {
    value: Value,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self::empty()
    }
}

impl AppConfig {
    /// Build an empty app configuration.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            value: Value::Object(serde_json::Map::new()),
        }
    }

    /// Build an app configuration from an untyped value supplied by the host.
    #[must_use]
    pub fn from_value(value: Value) -> Self {
        Self { value }
    }

    /// Return the raw configuration value.
    #[must_use]
    pub fn as_value(&self) -> &Value {
        &self.value
    }

    /// Deserialize this app configuration into the app's typed config struct.
    pub fn deserialize<T>(&self) -> Result<T, AppConfigError>
    where
        T: DeserializeOwned,
    {
        serde_json::from_value(self.value.clone()).map_err(AppConfigError::Deserialize)
    }
}

/// Configuration error returned by app providers.
#[derive(Debug, thiserror::Error)]
pub enum AppConfigError {
    /// The provider rejected or could not parse its configuration.
    #[error("invalid app config: {0}")]
    Deserialize(serde_json::Error),
    /// App-specific validation failed.
    #[error("invalid app config: {0}")]
    Invalid(String),
}

impl AppConfigError {
    /// Build an app-specific validation error.
    #[must_use]
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid(message.into())
    }
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;
    use serde_json::json;

    use super::*;

    #[derive(Debug, Deserialize, PartialEq, Eq)]
    #[serde(default, deny_unknown_fields)]
    struct TypedConfig {
        name: String,
        enabled: bool,
    }

    impl Default for TypedConfig {
        fn default() -> Self {
            Self {
                name: "default".to_string(),
                enabled: true,
            }
        }
    }

    #[test]
    fn empty_config_uses_typed_defaults() {
        let parsed: TypedConfig = AppConfig::empty().deserialize().unwrap();
        assert_eq!(parsed, TypedConfig::default());
    }

    #[test]
    fn unknown_fields_are_rejected_by_typed_config() {
        let err = AppConfig::from_value(json!({"unknown": true}))
            .deserialize::<TypedConfig>()
            .unwrap_err();
        assert!(err.to_string().contains("unknown field"));
    }
}
