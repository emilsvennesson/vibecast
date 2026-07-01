//! Discovery namespace messages (GET_DEVICE_INFO / DEVICE_INFO).

use serde::{Deserialize, Serialize};

/// GET_DEVICE_INFO request.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetDeviceInfoRequest {
    /// Request id.
    pub request_id: i64,
}

/// DEVICE_INFO response (values modeled after a real Chromecast / Shield TV).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInfoResponse {
    #[serde(rename = "type")]
    kind: &'static str,
    /// Request id.
    pub request_id: i64,
    /// Device id.
    pub device_id: String,
    /// Device model.
    pub device_model: String,
    /// Friendly name.
    pub friendly_name: String,
    /// Capability bitfield.
    pub device_capabilities: i64,
    /// Icon URL.
    pub device_icon_url: String,
    /// Control notifications flag.
    pub control_notifications: i64,
    /// Metrics id.
    pub receiver_metrics_id: String,
    /// Wifi proximity id.
    pub wifi_proximity_id: String,
}

impl DeviceInfoResponse {
    /// Build a DEVICE_INFO response with Cast-default fields.
    #[must_use]
    pub fn new(
        request_id: i64,
        device_id: String,
        device_model: String,
        friendly_name: String,
    ) -> Self {
        Self {
            kind: "DEVICE_INFO",
            request_id,
            device_id,
            device_model,
            friendly_name,
            device_capabilities: 4101,
            device_icon_url: String::new(),
            control_notifications: 1,
            receiver_metrics_id: String::new(),
            wifi_proximity_id: String::new(),
        }
    }
}
