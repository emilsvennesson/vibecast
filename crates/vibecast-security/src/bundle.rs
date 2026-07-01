//! [`CertificateBundle`]: all cryptographic material for one active certificate.

use std::time::{SystemTime, UNIX_EPOCH};

use md5::{Digest, Md5};

/// Current wall-clock time as whole seconds since the Unix epoch.
pub(crate) fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// All cryptographic material needed for Cast device authentication and TLS.
///
/// Device-auth signatures are *pre-computed* (harvested from a real device) and
/// stored verbatim; the receiver never signs a challenge at runtime.
#[derive(Clone)]
pub struct CertificateBundle {
    /// TLS server certificate (PEM).
    pub peer_cert_pem: Vec<u8>,
    /// TLS server private key (PEM; PKCS#1, PKCS#8, or SEC1).
    pub peer_key_pem: Vec<u8>,
    /// Peer certificate in DER (derived from `peer_cert_pem` at load time).
    pub peer_cert_der: Vec<u8>,
    /// Manufacturing device certificate (DER).
    pub device_cert_der: Vec<u8>,
    /// Intermediate CA chain, each certificate in DER.
    pub intermediate_certs_der: Vec<Vec<u8>>,
    /// Pre-computed auth signature for a SHA-1 challenge.
    pub signature_sha1: Vec<u8>,
    /// Pre-computed auth signature for a SHA-256 challenge.
    pub signature_sha256: Vec<u8>,
    /// Peer certificate validity start (Unix seconds).
    pub not_valid_before: i64,
    /// Peer certificate validity end (Unix seconds).
    pub not_valid_after: i64,
    /// Optional CRL payload included in auth responses.
    pub crl: Option<Vec<u8>>,
}

impl CertificateBundle {
    /// Whether this peer certificate is valid at `unix_secs`.
    #[must_use]
    pub fn is_valid_at(&self, unix_secs: i64) -> bool {
        self.not_valid_before <= unix_secs && unix_secs <= self.not_valid_after
    }

    /// Whether this peer certificate is valid right now.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.is_valid_at(now_unix())
    }

    /// MD5 hex digest of the peer certificate DER, used for the mDNS `cd` field.
    #[must_use]
    pub fn cert_digest_md5(&self) -> String {
        hex::encode(Md5::digest(&self.peer_cert_der))
    }
}

impl std::fmt::Debug for CertificateBundle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Avoid dumping key/cert/signature bytes into logs.
        f.debug_struct("CertificateBundle")
            .field("not_valid_before", &self.not_valid_before)
            .field("not_valid_after", &self.not_valid_after)
            .field("intermediates", &self.intermediate_certs_der.len())
            .field("has_crl", &self.crl.is_some())
            .finish()
    }
}
