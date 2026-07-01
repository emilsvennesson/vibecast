//! Error type for discovery services.

/// Errors raised by mDNS advertisement or the eureka server.
#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    /// mDNS daemon or service registration failure.
    #[error("mDNS error: {0}")]
    Mdns(String),
    /// Network/serving I/O failure.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// Certificate material failure (e.g. extracting the device public key).
    #[error(transparent)]
    Security(#[from] vibecast_security::SecurityError),
}
