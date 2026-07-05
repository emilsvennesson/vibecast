//! TLS server configuration with in-memory, hot-swappable certificates.
//!
//! A [`CertResolver`] holds the current [`CertifiedKey`] behind an [`ArcSwap`],
//! so certificate rotation is a lock-free atomic store with zero downtime and
//! no temp files.

use std::sync::Arc;

use arc_swap::ArcSwap;
use rustls::server::{ClientHello, ResolvesServerCert, ServerConfig};
use rustls::sign::CertifiedKey;
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};

use crate::bundle::CertificateBundle;
use crate::error::SecurityError;

/// A `ResolvesServerCert` that serves the current certificate and supports
/// atomic hot-reload via [`CertResolver::update`].
pub struct CertResolver {
    current: ArcSwap<CertifiedKey>,
}

impl std::fmt::Debug for CertResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CertResolver").finish_non_exhaustive()
    }
}

impl CertResolver {
    /// Build a resolver from the given bundle.
    pub fn new(bundle: &CertificateBundle) -> Result<Arc<Self>, SecurityError> {
        Ok(Arc::new(Self {
            current: ArcSwap::from_pointee(certified_key(bundle)?),
        }))
    }

    /// Atomically replace the served certificate (used on rotation).
    pub fn update(&self, bundle: &CertificateBundle) -> Result<(), SecurityError> {
        self.current.store(Arc::new(certified_key(bundle)?));
        Ok(())
    }
}

impl ResolvesServerCert for CertResolver {
    fn resolve(&self, _client_hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        Some(self.current.load_full())
    }
}

/// Build a `ServerConfig` that resolves its certificate via `resolver`.
///
/// No client authentication is required (matching a real Cast receiver).
pub fn server_config(resolver: Arc<dyn ResolvesServerCert>) -> Result<ServerConfig, SecurityError> {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| SecurityError::Tls(e.to_string()))?
        .with_no_client_auth()
        .with_cert_resolver(resolver);
    Ok(config)
}

fn certified_key(bundle: &CertificateBundle) -> Result<CertifiedKey, SecurityError> {
    let cert_chain = vec![CertificateDer::from(bundle.peer_cert_der.clone())];
    let key = load_private_key(&bundle.peer_key_pem)?;
    let signing_key = rustls::crypto::aws_lc_rs::sign::any_supported_type(&key)
        .map_err(|e| SecurityError::Tls(format!("unsupported private key: {e}")))?;
    Ok(CertifiedKey::new(cert_chain, signing_key))
}

fn load_private_key(pem: &[u8]) -> Result<PrivateKeyDer<'static>, SecurityError> {
    // Parses the first PKCS#8/PKCS#1/SEC1 private-key block (rustls-pki-types'
    // PemObject replaces the now-unmaintained rustls-pemfile crate).
    PrivateKeyDer::from_pem_slice(pem)
        .map_err(|e| SecurityError::Tls(format!("private key read: {e}")))
}
