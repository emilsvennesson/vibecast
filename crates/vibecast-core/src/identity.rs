//! Device identity used across the runtime.

/// Identity/configuration fields for the receiver.
#[derive(Debug, Clone)]
pub struct DeviceIdentity {
    /// Friendly name shown to senders.
    pub friendly_name: String,
    /// Device model string.
    pub device_model: String,
    /// Stable device id.
    pub device_id: String,
    /// SSDP UDN (defaults to `device_id`).
    pub ssdp_udn: String,
}

impl DeviceIdentity {
    /// Build an identity, defaulting `ssdp_udn` to `device_id`.
    #[must_use]
    pub fn new(friendly_name: String, device_model: String, device_id: String) -> Self {
        Self {
            ssdp_udn: device_id.clone(),
            friendly_name,
            device_model,
            device_id,
        }
    }
}
