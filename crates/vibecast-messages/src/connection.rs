//! Connection namespace messages (CONNECT / CLOSE).

use serde::Deserialize;
use serde_json::Value;

/// Metadata about the connecting sender (embedded in CONNECT).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SenderInfo {
    /// SDK type.
    pub sdk_type: Option<i64>,
    /// SDK version.
    pub version: Option<String>,
    /// Browser version.
    pub browser_version: Option<String>,
    /// Platform code.
    pub platform: Option<i64>,
    /// Connection type code.
    pub connection_type: Option<i64>,
    /// Sender model.
    pub model: Option<String>,
    /// System version.
    pub system_version: Option<String>,
}

/// Virtual connection request from a sender.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectRequest {
    /// Origin object (opaque).
    #[serde(default)]
    pub origin: Value,
    /// Sender user agent.
    pub user_agent: Option<String>,
    /// Sender metadata.
    pub sender_info: Option<SenderInfo>,
    /// Connection type (numeric in some senders).
    pub conn_type: Option<i64>,
}

/// Virtual connection close from a sender.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloseRequest {
    /// Reason code.
    pub reason_code: Option<i64>,
}

/// Discriminated union of connection namespace messages.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum ConnectionMessage {
    /// CONNECT.
    #[serde(rename = "CONNECT")]
    Connect(ConnectRequest),
    /// CLOSE.
    #[serde(rename = "CLOSE")]
    Close(CloseRequest),
}
