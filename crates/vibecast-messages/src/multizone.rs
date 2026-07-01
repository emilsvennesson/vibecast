//! Multizone namespace messages (GET_STATUS / MULTIZONE_STATUS).

use serde::{Deserialize, Serialize};

/// Multizone GET_STATUS request.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MultizoneGetStatusRequest {
    /// Request id.
    pub request_id: i64,
}

/// Multizone status payload (empty for a standalone receiver).
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MultizoneStatus {
    /// Member devices.
    pub devices: Vec<serde_json::Value>,
    /// Whether multichannel.
    pub is_multichannel: bool,
}

/// MULTIZONE_STATUS response.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MultizoneStatusResponse {
    #[serde(rename = "type")]
    kind: &'static str,
    /// Request id.
    pub request_id: i64,
    /// Status payload.
    pub status: MultizoneStatus,
}

impl MultizoneStatusResponse {
    /// Build an empty MULTIZONE_STATUS response.
    #[must_use]
    pub fn empty(request_id: i64) -> Self {
        Self {
            kind: "MULTIZONE_STATUS",
            request_id,
            status: MultizoneStatus::default(),
        }
    }
}
