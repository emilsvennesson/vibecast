//! Per-session app context.

use std::path::PathBuf;

/// Receiver metadata made available to app sessions.
#[derive(Debug, Clone)]
pub struct ReceiverContext {
    /// Receiver friendly name.
    pub friendly_name: String,
    /// Device model string.
    pub device_model: String,
    /// Stable device id.
    pub device_id: String,
    /// Per-app data directory.
    pub data_dir: PathBuf,
    /// User-agent header apps should send.
    pub user_agent: String,
    /// Cast device-capabilities header value.
    pub cast_device_capabilities: String,
    /// Display width in pixels.
    pub display_width: u32,
    /// Display height in pixels.
    pub display_height: u32,
}

impl ReceiverContext {
    /// Build a receiver context with default display and empty header hints.
    #[must_use]
    pub fn new(
        friendly_name: impl Into<String>,
        device_model: impl Into<String>,
        device_id: impl Into<String>,
        data_dir: PathBuf,
    ) -> Self {
        Self {
            friendly_name: friendly_name.into(),
            device_model: device_model.into(),
            device_id: device_id.into(),
            data_dir,
            user_agent: String::new(),
            cast_device_capabilities: String::new(),
            display_width: 1920,
            display_height: 1080,
        }
    }
}

/// Context passed to app callbacks for one session.
#[derive(Debug, Clone)]
pub struct AppContext {
    /// Session id.
    pub session_id: String,
    /// Transport id for the session.
    pub transport_id: String,
    /// Launched Cast app id.
    pub app_id: String,
    /// Shared HTTP client for app API calls.
    pub http: reqwest::Client,
    /// Receiver metadata.
    pub receiver: ReceiverContext,
}
