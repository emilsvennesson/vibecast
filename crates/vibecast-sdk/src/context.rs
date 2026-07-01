//! Per-session app context and the custom-message sender seam.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

/// Sends custom-namespace messages on behalf of an app callback.
///
/// The coordinator supplies a per-callback implementation that writes directly
/// to the relevant sender connection(s), so it never routes back through the
/// hub mailbox that is awaiting the callback.
#[async_trait]
pub trait SenderChannel: Send + Sync {
    /// Send to the sender that triggered the callback (broadcasts if unbound).
    async fn send_custom(&self, namespace: &str, data: Value);
    /// Broadcast to all senders subscribed to this transport.
    async fn broadcast_custom(&self, namespace: &str, data: Value);
}

/// A no-op channel for contexts without a live transport (tests, teardown).
pub struct NoopSenderChannel;

#[async_trait]
impl SenderChannel for NoopSenderChannel {
    async fn send_custom(&self, _namespace: &str, _data: Value) {}
    async fn broadcast_custom(&self, _namespace: &str, _data: Value) {}
}

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
#[derive(Clone)]
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
    sender: Arc<dyn SenderChannel>,
}

impl AppContext {
    /// Build a context bound to the given custom-message sender channel.
    #[must_use]
    pub fn new(
        session_id: impl Into<String>,
        transport_id: impl Into<String>,
        app_id: impl Into<String>,
        http: reqwest::Client,
        receiver: ReceiverContext,
        sender: Arc<dyn SenderChannel>,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            transport_id: transport_id.into(),
            app_id: app_id.into(),
            http,
            receiver,
            sender,
        }
    }

    /// Send a custom-namespace message to the sender associated with this
    /// callback (broadcasts if there is no bound sender).
    pub async fn send_custom(&self, namespace: &str, data: Value) {
        self.sender.send_custom(namespace, data).await;
    }

    /// Broadcast a custom-namespace message to all senders on this transport.
    pub async fn broadcast_custom(&self, namespace: &str, data: Value) {
        self.sender.broadcast_custom(namespace, data).await;
    }
}
