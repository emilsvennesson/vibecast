//! Error type for certificate loading and TLS setup.

/// Errors raised while loading certificate material or configuring TLS.
#[derive(Debug, thiserror::Error)]
pub enum SecurityError {
    /// Reading the manifest file failed.
    #[error("manifest i/o error: {0}")]
    Io(#[from] std::io::Error),
    /// The manifest was not valid JSON.
    #[error("manifest json error: {0}")]
    Json(#[from] serde_json::Error),
    /// A manifest field was missing, empty, or otherwise invalid.
    #[error("manifest field `{field}` invalid: {reason}")]
    Manifest {
        /// The offending field name.
        field: &'static str,
        /// Why it was rejected.
        reason: String,
    },
    /// A base64 value in the manifest could not be decoded.
    #[error("invalid base64 in `{field}`: {source}")]
    Base64 {
        /// The offending field name.
        field: &'static str,
        /// The underlying decode error.
        #[source]
        source: base64::DecodeError,
    },
    /// A PEM/DER certificate in the manifest could not be parsed.
    #[error("certificate parse error in `{field}`: {reason}")]
    Cert {
        /// The offending field name.
        field: &'static str,
        /// The parse failure detail.
        reason: String,
    },
    /// The manifest contains no certificate valid at the current time.
    #[error("no currently valid certificate in manifest")]
    NoValidCert,
    /// TLS configuration (key loading, provider setup) failed.
    #[error("tls configuration error: {0}")]
    Tls(String),
}
