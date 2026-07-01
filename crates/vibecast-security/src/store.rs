//! [`CertificateStore`]: manifest loading and active-certificate selection.

use std::path::Path;

use base64::prelude::{Engine, BASE64_STANDARD};
use serde::Deserialize;

use crate::bundle::{now_unix, CertificateBundle};
use crate::error::SecurityError;

/// A collection of peer certificates with automatic active-cert selection.
///
/// The manifest holds one device/intermediate chain shared by many peer
/// certificates, each with its own validity window and pre-computed signatures.
#[derive(Debug)]
pub struct CertificateStore {
    bundles: Vec<CertificateBundle>,
    active: usize,
}

impl CertificateStore {
    /// Build a store from pre-constructed bundles, selecting the one valid now.
    pub fn new(mut bundles: Vec<CertificateBundle>) -> Result<Self, SecurityError> {
        if bundles.is_empty() {
            return Err(SecurityError::Manifest {
                field: "certs",
                reason: "at least one certificate is required".into(),
            });
        }
        bundles.sort_by_key(|b| b.not_valid_before);
        let active = find_valid(&bundles, now_unix()).ok_or(SecurityError::NoValidCert)?;
        Ok(Self { bundles, active })
    }

    /// Load a store from a manifest JSON file.
    pub fn from_manifest_path(path: impl AsRef<Path>) -> Result<Self, SecurityError> {
        let raw = std::fs::read_to_string(path)?;
        Self::from_manifest_str(&raw)
    }

    /// Load a store from manifest JSON text.
    ///
    /// Expected shape: `{"cpu": <PEM>, "ica": <PEM>, "certs": [{...}], "crl"?: <b64>}`.
    pub fn from_manifest_str(json: &str) -> Result<Self, SecurityError> {
        let manifest: RawManifest = serde_json::from_str(json)?;
        Self::new(manifest.into_bundles()?)
    }

    /// The currently selected valid bundle.
    #[must_use]
    pub fn active_bundle(&self) -> &CertificateBundle {
        &self.bundles[self.active]
    }

    /// All loaded bundles, sorted by validity start.
    #[must_use]
    pub fn bundles(&self) -> &[CertificateBundle] {
        &self.bundles
    }

    /// Rotate to a newly valid certificate if the active one has expired.
    ///
    /// Returns the new active bundle when a rotation occurred, else `None`.
    pub fn rotate_if_needed(
        &mut self,
        now: i64,
    ) -> Result<Option<&CertificateBundle>, SecurityError> {
        if self.bundles[self.active].is_valid_at(now) {
            return Ok(None);
        }
        let next = find_valid(&self.bundles, now).ok_or(SecurityError::NoValidCert)?;
        if next == self.active {
            return Ok(None);
        }
        self.active = next;
        Ok(Some(&self.bundles[self.active]))
    }
}

fn find_valid(bundles: &[CertificateBundle], now: i64) -> Option<usize> {
    bundles.iter().position(|b| b.is_valid_at(now))
}

// --- Manifest wire format --------------------------------------------------

#[derive(Deserialize)]
struct RawManifest {
    cpu: String,
    ica: String,
    certs: Vec<RawCert>,
    #[serde(default)]
    crl: Option<String>,
}

#[derive(Deserialize)]
struct RawCert {
    pu: String,
    pr: String,
    sig_sha1: String,
    sig_sha256: String,
}

impl RawManifest {
    fn into_bundles(self) -> Result<Vec<CertificateBundle>, SecurityError> {
        require_non_empty(&self.cpu, "cpu")?;
        require_non_empty(&self.ica, "ica")?;
        if self.certs.is_empty() {
            return Err(SecurityError::Manifest {
                field: "certs",
                reason: "must be a non-empty list".into(),
            });
        }

        let device_cert_der = first_pem_der(self.cpu.as_bytes(), "cpu")?;
        let intermediate_certs_der = all_pem_ders(self.ica.as_bytes(), "ica")?;
        let crl = match self.crl {
            None => None,
            Some(ref s) => {
                require_non_empty(s, "crl")?;
                Some(decode_b64(s, "crl")?)
            }
        };

        let mut bundles = Vec::with_capacity(self.certs.len());
        for cert in self.certs {
            require_non_empty(&cert.pu, "pu")?;
            require_non_empty(&cert.pr, "pr")?;
            require_non_empty(&cert.sig_sha1, "sig_sha1")?;
            require_non_empty(&cert.sig_sha256, "sig_sha256")?;

            let peer_cert_der = first_pem_der(cert.pu.as_bytes(), "pu")?;
            let (not_valid_before, not_valid_after) = cert_validity(&peer_cert_der, "pu")?;

            bundles.push(CertificateBundle {
                peer_cert_pem: cert.pu.into_bytes(),
                peer_key_pem: cert.pr.into_bytes(),
                peer_cert_der,
                device_cert_der: device_cert_der.clone(),
                intermediate_certs_der: intermediate_certs_der.clone(),
                signature_sha1: decode_b64(&cert.sig_sha1, "sig_sha1")?,
                signature_sha256: decode_b64(&cert.sig_sha256, "sig_sha256")?,
                not_valid_before,
                not_valid_after,
                crl: crl.clone(),
            });
        }
        Ok(bundles)
    }
}

fn require_non_empty(value: &str, field: &'static str) -> Result<(), SecurityError> {
    if value.is_empty() {
        return Err(SecurityError::Manifest {
            field,
            reason: "must be a non-empty string".into(),
        });
    }
    Ok(())
}

fn decode_b64(value: &str, field: &'static str) -> Result<Vec<u8>, SecurityError> {
    BASE64_STANDARD
        .decode(value.as_bytes())
        .map_err(|source| SecurityError::Base64 { field, source })
}

/// Decode all PEM blocks in `pem` into their DER contents.
fn all_pem_ders(pem: &[u8], field: &'static str) -> Result<Vec<Vec<u8>>, SecurityError> {
    let mut ders = Vec::new();
    for item in x509_parser::pem::Pem::iter_from_buffer(pem) {
        let block = item.map_err(|e| SecurityError::Cert {
            field,
            reason: e.to_string(),
        })?;
        ders.push(block.contents);
    }
    if ders.is_empty() {
        return Err(SecurityError::Cert {
            field,
            reason: "no PEM certificate found".into(),
        });
    }
    Ok(ders)
}

fn first_pem_der(pem: &[u8], field: &'static str) -> Result<Vec<u8>, SecurityError> {
    all_pem_ders(pem, field).map(|mut ders| ders.swap_remove(0))
}

fn cert_validity(der: &[u8], field: &'static str) -> Result<(i64, i64), SecurityError> {
    let (_, cert) = x509_parser::parse_x509_certificate(der).map_err(|e| SecurityError::Cert {
        field,
        reason: e.to_string(),
    })?;
    let validity = cert.validity();
    Ok((
        validity.not_before.timestamp(),
        validity.not_after.timestamp(),
    ))
}
