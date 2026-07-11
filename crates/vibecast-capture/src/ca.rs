//! Per-host leaf minting for the HTTPS MITM, signed by a **pre-trusted** CA.
//!
//! Certificate authority setup is intentionally *outside* this tool: the
//! operator installs a MITM CA into the device's system trust store once (e.g.
//! a Magisk cert module, trusted from boot by every app). This module loads
//! that CA's certificate + private key and mints short-lived per-host leaves on
//! demand, so apps that don't pin trust our intercepted TLS with no per-session
//! device changes (no force-stop, no runtime cert mount).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use rcgen::{
    date_time_ymd, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa,
    KeyPair, KeyUsagePurpose,
};
use rustls::ServerConfig;
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};

use crate::error::CaptureError;

/// A loaded CA plus a cache of minted per-host TLS server configs.
pub struct CaptureCa {
    issuer: rcgen::Certificate,
    issuer_key: KeyPair,
    cache: Mutex<HashMap<String, Arc<ServerConfig>>>,
}

impl CaptureCa {
    /// Load a CA from its certificate + private-key PEM (e.g. the two blocks of
    /// `~/.mitmproxy/mitmproxy-ca.pem`). The private key may be PKCS#8 or PKCS#1.
    pub fn from_pem(cert_pem: &str, key_pem: &str) -> Result<Self, CaptureError> {
        let issuer_key = load_key(key_pem)?;
        let params = CertificateParams::from_ca_cert_pem(cert_pem)
            .map_err(|e| CaptureError::Ca(format!("parse CA certificate: {e}")))?;
        // `signed_by` reads only the issuer's DN + key usages from this object;
        // the leaf is verified by clients against the real CA in their store.
        let issuer = params
            .self_signed(&issuer_key)
            .map_err(|e| CaptureError::Ca(format!("prepare CA issuer: {e}")))?;
        Ok(Self {
            issuer,
            issuer_key,
            cache: Mutex::new(HashMap::new()),
        })
    }

    /// Return (minting + caching) a rustls `ServerConfig` for `host`.
    pub fn server_config(&self, host: &str) -> Result<Arc<ServerConfig>, CaptureError> {
        if let Some(cfg) = self
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(host)
        {
            return Ok(cfg.clone());
        }
        let cfg = Arc::new(self.mint(host)?);
        self.cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(host.to_owned(), cfg.clone());
        Ok(cfg)
    }

    fn mint(&self, host: &str) -> Result<ServerConfig, CaptureError> {
        let leaf_key = KeyPair::generate().map_err(|e| CaptureError::Ca(e.to_string()))?;
        let mut params = CertificateParams::new(vec![host.to_owned()])
            .map_err(|e| CaptureError::Ca(e.to_string()))?;
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, host);
        params.distinguished_name = dn;
        params.is_ca = IsCa::NoCa;
        params.use_authority_key_identifier_extension = true;
        params.key_usages = vec![
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::KeyEncipherment,
        ];
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        // Keep total validity under ~398 days: Chromium's network stack (cronet,
        // used by mediashell/Cast) rejects longer-lived leaves outright, which
        // would fail every host regardless of CA trust.
        let (yb, mb, db) = civil_from_now(-1);
        let (ya, ma, da) = civil_from_now(396);
        params.not_before = date_time_ymd(yb, mb, db);
        params.not_after = date_time_ymd(ya, ma, da);

        let leaf = params
            .signed_by(&leaf_key, &self.issuer, &self.issuer_key)
            .map_err(|e| CaptureError::Ca(e.to_string()))?;

        // Leaf only: clients build the path to the (already trusted) root.
        let chain = vec![leaf.der().clone()];
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(leaf_key.serialize_der()));

        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let mut config = ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|e| CaptureError::Tls(e.to_string()))?
            .with_no_client_auth()
            .with_single_cert(chain, key)
            .map_err(|e| CaptureError::Tls(e.to_string()))?;
        config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        Ok(config)
    }
}

/// Civil `(year, month, day)` for `days_from_now` days relative to today (UTC).
fn civil_from_now(days_from_now: i64) -> (i32, u8, u8) {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    // Howard Hinnant's civil-from-days.
    let z = now_secs / 86_400 + days_from_now + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    (year as i32, month as u8, day as u8)
}

/// Load a CA private key (PKCS#8 or PKCS#1 RSA) into an rcgen `KeyPair`.
fn load_key(key_pem: &str) -> Result<KeyPair, CaptureError> {
    let der = PrivateKeyDer::from_pem_slice(key_pem.as_bytes())
        .map_err(|e| CaptureError::Ca(format!("parse CA key: {e}")))?;
    let pkcs8: Vec<u8> = match der {
        PrivateKeyDer::Pkcs8(k) => k.secret_pkcs8_der().to_vec(),
        PrivateKeyDer::Pkcs1(k) => pkcs1_to_pkcs8(k.secret_pkcs1_der()),
        PrivateKeyDer::Sec1(_) => {
            return Err(CaptureError::Ca(
                "SEC1 EC CA keys are not supported; provide a PKCS#8 or RSA key".into(),
            ))
        }
        _ => return Err(CaptureError::Ca("unrecognized CA key format".into())),
    };
    KeyPair::try_from(pkcs8.as_slice()).map_err(|e| CaptureError::Ca(format!("load CA key: {e}")))
}

/// Wrap a PKCS#1 `RSAPrivateKey` DER in an unencrypted PKCS#8 `PrivateKeyInfo`.
fn pkcs1_to_pkcs8(pkcs1: &[u8]) -> Vec<u8> {
    // AlgorithmIdentifier { rsaEncryption (1.2.840.113549.1.1.1), NULL }.
    const RSA_ALG_ID: [u8; 15] = [
        0x30, 0x0d, 0x06, 0x09, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01, 0x05, 0x00,
    ];
    let mut body = Vec::new();
    body.extend_from_slice(&[0x02, 0x01, 0x00]); // version 0
    body.extend_from_slice(&RSA_ALG_ID);
    body.extend(der_tlv(0x04, pkcs1)); // privateKey OCTET STRING
    der_tlv(0x30, &body) // outer SEQUENCE
}

fn der_tlv(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    let len = content.len();
    if len < 0x80 {
        out.push(len as u8);
    } else {
        let bytes = len.to_be_bytes();
        let start = bytes
            .iter()
            .position(|&b| b != 0)
            .unwrap_or(bytes.len() - 1);
        let sig = &bytes[start..];
        out.push(0x80 | sig.len() as u8);
        out.extend_from_slice(sig);
    }
    out.extend_from_slice(content);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Load a CA from PEM (PKCS#8 path) and mint a cached per-host leaf.
    #[test]
    fn loads_ca_from_pem_and_mints_cached_leaf() {
        let ca_key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, "test ca");
        params.distinguished_name = dn;
        params.is_ca = IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign];
        let ca_cert = params.self_signed(&ca_key).unwrap();

        let ca = CaptureCa::from_pem(&ca_cert.pem(), &ca_key.serialize_pem()).unwrap();
        let a = ca.server_config("example.com").unwrap();
        let b = ca.server_config("example.com").unwrap();
        assert!(Arc::ptr_eq(&a, &b)); // cached
        let _ = ca.server_config("other.test").unwrap();
    }

    /// End-to-end: mint a leaf with the real (RSA, PKCS#1) mitmproxy CA and
    /// confirm a rustls client that trusts that CA accepts the handshake.
    /// Skipped when the mitmproxy CA isn't present on this machine.
    #[tokio::test]
    async fn mints_leaf_trusted_by_real_mitmproxy_ca() {
        use rustls_pki_types::{CertificateDer, ServerName};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

        let home = std::env::var("HOME").unwrap_or_default();
        let path = format!("{home}/.mitmproxy/mitmproxy-ca.pem");
        let Ok(pem) = std::fs::read_to_string(&path) else {
            eprintln!("skip: {path} not present");
            return;
        };
        let cert_pem = block(&pem, "CERTIFICATE");
        let key_pem = block(&pem, "PRIVATE KEY"); // matches "RSA PRIVATE KEY" too

        let ca = CaptureCa::from_pem(&cert_pem, &key_pem).expect("load CA");
        let server_config = ca.server_config("example.com").expect("mint leaf");

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(server_config);
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            if let Ok(mut tls) = acceptor.accept(stream).await {
                let _ = tls.write_all(b"ok").await;
                let _ = tls.shutdown().await;
            }
        });

        // Client trusts ONLY the mitmproxy CA.
        let mut roots = rustls::RootCertStore::empty();
        let ca_der = CertificateDer::from_pem_slice(cert_pem.as_bytes()).unwrap();
        roots.add(ca_der).unwrap();
        let client_config = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let connector = tokio_rustls::TlsConnector::from(Arc::new(client_config));
        let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let name = ServerName::try_from("example.com").unwrap();

        let mut tls = connector
            .connect(name, stream)
            .await
            .expect("client must trust the minted leaf");
        let mut buf = Vec::new();
        tls.read_to_end(&mut buf).await.unwrap();
        assert_eq!(buf, b"ok");
    }

    fn block(pem: &str, kind: &str) -> String {
        let begin = "-----BEGIN ";
        let mut rest = pem;
        while let Some(s) = rest.find(begin) {
            let he = rest[s..].find('\n').unwrap() + s;
            let label = rest[s + begin.len()..he].trim_end_matches('-');
            if label.contains(kind) {
                let end = format!("-----END {label}-----");
                let e = rest[he..].find(&end).unwrap() + he + end.len();
                return rest[s..e].to_string();
            }
            rest = &rest[he..];
        }
        panic!("no {kind} block");
    }

    #[test]
    fn pkcs1_wrapper_is_well_formed_der() {
        // Short-form length.
        let short = pkcs1_to_pkcs8(&[0xAA; 8]);
        assert_eq!(short[0], 0x30); // outer SEQUENCE
        assert_eq!(short[2], 0x02); // version INTEGER
                                    // Long-form length (content > 127 bytes).
        let long = pkcs1_to_pkcs8(&[0xAA; 200]);
        assert_eq!(long[0], 0x30);
        assert_eq!(long[1] & 0x80, 0x80); // long-form length marker
    }
}
