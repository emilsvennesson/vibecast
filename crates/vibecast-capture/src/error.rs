//! Error type for the capture proxy.

/// Failures that abort a capture session before/while it runs.
#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    /// Filesystem I/O (session dir, log files, CA material).
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// Loading the harvested certificate manifest failed.
    #[error("certificate manifest: {0}")]
    Security(#[from] vibecast_security::SecurityError),

    /// mDNS advertisement failed.
    #[error("mdns: {0}")]
    Discovery(#[from] vibecast_discovery::DiscoveryError),

    /// Building the sender-facing TLS config failed.
    #[error("tls: {0}")]
    Tls(String),

    /// Certificate-authority / leaf minting failed.
    #[error("certificate authority: {0}")]
    Ca(String),

    /// An `adb` invocation failed (device offline, no root, bad command).
    #[error("adb: {0}")]
    Adb(String),

    /// Serializing a capture record to JSON failed.
    #[error("serialize: {0}")]
    Serialize(#[from] serde_json::Error),
}
