//! Setup namespace messages (eureka_info). Note: this namespace uses
//! snake_case field names on the wire, unlike the others.

use serde::{Deserialize, Serialize};

/// Inbound setup request.
#[derive(Debug, Clone, Deserialize)]
pub struct SetupRequest {
    /// Request id (accepts both `request_id` and `requestId`).
    #[serde(alias = "requestId")]
    pub request_id: i64,
}

/// Setup device-info block.
#[derive(Debug, Clone, Serialize)]
pub struct SetupDeviceInfo {
    /// SSDP UDN.
    pub ssdp_udn: String,
}

/// Setup response data.
#[derive(Debug, Clone, Serialize)]
pub struct SetupData {
    /// Device info.
    pub device_info: SetupDeviceInfo,
    /// Friendly name.
    pub name: String,
    /// Setup protocol version.
    pub version: i64,
}

/// Outbound setup response.
#[derive(Debug, Clone, Serialize)]
pub struct SetupResponse {
    #[serde(rename = "type")]
    kind: &'static str,
    /// Request id.
    pub request_id: i64,
    /// HTTP-like response code.
    pub response_code: i64,
    /// Response string.
    pub response_string: String,
    /// Response data.
    pub data: SetupData,
}

impl SetupResponse {
    /// Build an OK setup response.
    #[must_use]
    pub fn ok(request_id: i64, name: String, ssdp_udn: String) -> Self {
        Self {
            kind: "eureka_info",
            request_id,
            response_code: 200,
            response_string: "OK".into(),
            data: SetupData {
                device_info: SetupDeviceInfo { ssdp_udn },
                name,
                version: 8,
            },
        }
    }
}
