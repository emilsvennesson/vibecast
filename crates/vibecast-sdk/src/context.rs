//! Per-session app context and the custom-message sender seam.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Serialize;
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
    ///
    /// Accepts any [`Serialize`] value, so apps can pass a typed message struct
    /// or a `serde_json::Value`. A serialization failure is logged rather than
    /// silently dropped.
    pub async fn send_custom<T: Serialize>(&self, namespace: &str, message: T) {
        match serde_json::to_value(&message) {
            Ok(value) => self.sender.send_custom(namespace, value).await,
            Err(error) => {
                tracing::error!(%error, namespace, "failed to serialize outbound app message");
            }
        }
    }

    /// Broadcast a custom-namespace message to all senders on this transport.
    ///
    /// Accepts any [`Serialize`] value; see [`send_custom`](Self::send_custom).
    pub async fn broadcast_custom<T: Serialize>(&self, namespace: &str, message: T) {
        match serde_json::to_value(&message) {
            Ok(value) => self.sender.broadcast_custom(namespace, value).await,
            Err(error) => {
                tracing::error!(%error, namespace, "failed to serialize outbound app message");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;
    use serde_json::{json, Value};
    use std::sync::Mutex;

    #[derive(Default)]
    struct CapturingChannel {
        sent: Mutex<Vec<(String, Value)>>,
        broadcast: Mutex<Vec<(String, Value)>>,
    }

    #[async_trait]
    impl SenderChannel for CapturingChannel {
        async fn send_custom(&self, namespace: &str, data: Value) {
            self.sent
                .lock()
                .expect("sent not poisoned")
                .push((namespace.to_string(), data));
        }
        async fn broadcast_custom(&self, namespace: &str, data: Value) {
            self.broadcast
                .lock()
                .expect("broadcast not poisoned")
                .push((namespace.to_string(), data));
        }
    }

    fn ctx(channel: Arc<CapturingChannel>) -> AppContext {
        AppContext::new(
            "s1",
            "t1",
            "APP",
            reqwest::Client::new(),
            ReceiverContext::new("vibecast", "Model", "device-1", PathBuf::new()),
            channel,
        )
    }

    #[derive(Serialize)]
    struct StatusMsg {
        status: &'static str,
        progress: f64,
    }

    #[tokio::test]
    async fn send_custom_forwards_typed_payload_as_json() {
        let channel = Arc::new(CapturingChannel::default());
        let ctx = ctx(channel.clone());
        ctx.send_custom(
            "urn:test:status",
            StatusMsg {
                status: "PLAYING",
                progress: 0.5,
            },
        )
        .await;
        let sent = channel.sent.lock().expect("sent not poisoned");
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0, "urn:test:status");
        assert_eq!(sent[0].1, json!({"status":"PLAYING","progress":0.5}));
    }

    #[tokio::test]
    async fn broadcast_custom_forwards_typed_payload_as_json() {
        let channel = Arc::new(CapturingChannel::default());
        let ctx = ctx(channel.clone());
        ctx.broadcast_custom(
            "urn:test:broadcast",
            StatusMsg {
                status: "IDLE",
                progress: 1.0,
            },
        )
        .await;
        let broadcast = channel.broadcast.lock().expect("broadcast not poisoned");
        assert_eq!(broadcast.len(), 1);
        assert_eq!(broadcast[0].0, "urn:test:broadcast");
        assert_eq!(broadcast[0].1, json!({"status":"IDLE","progress":1.0}));
        assert!(channel.sent.lock().expect("sent not poisoned").is_empty());
    }
}
