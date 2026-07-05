//! Tests for manifest loading, device-auth message building, rotation, and TLS.

use base64::prelude::{Engine, BASE64_STANDARD};
use prost::Message;
use vibecast_proto::DeviceAuthMessage;

use super::*;

/// Generate a self-signed cert with explicit validity. Returns (cert PEM, key PEM, cert DER).
fn cert(cn: &str, nb: (i32, u8, u8), na: (i32, u8, u8)) -> (String, String, Vec<u8>) {
    let key = rcgen::KeyPair::generate().unwrap();
    let mut params = rcgen::CertificateParams::new(vec![cn.to_string()]).unwrap();
    params.not_before = rcgen::date_time_ymd(nb.0, nb.1, nb.2);
    params.not_after = rcgen::date_time_ymd(na.0, na.1, na.2);
    let certificate = params.self_signed(&key).unwrap();
    (
        certificate.pem(),
        key.serialize_pem(),
        certificate.der().to_vec(),
    )
}

fn unix(y: i32, m: u8, d: u8) -> i64 {
    rcgen::date_time_ymd(y, m, d).unix_timestamp()
}

/// Build manifest JSON. Each cert entry is (peer PEM, key PEM, sig_sha1, sig_sha256).
fn manifest(
    device_pem: &str,
    ica_pem: &str,
    certs: &[(String, String, Vec<u8>, Vec<u8>)],
    crl: Option<Vec<u8>>,
) -> String {
    let certs_json: Vec<_> = certs
        .iter()
        .map(|(pu, pr, s1, s256)| {
            serde_json::json!({
                "pu": pu,
                "pr": pr,
                "sig_sha1": BASE64_STANDARD.encode(s1),
                "sig_sha256": BASE64_STANDARD.encode(s256),
            })
        })
        .collect();
    let mut obj = serde_json::json!({ "cpu": device_pem, "ica": ica_pem, "certs": certs_json });
    if let Some(c) = crl {
        obj["crl"] = serde_json::json!(BASE64_STANDARD.encode(c));
    }
    obj.to_string()
}

#[test]
fn loads_manifest_and_selects_active_bundle() {
    let (dev, _dk, dev_der) = cert("Device", (2000, 1, 1), (2099, 1, 1));
    let (ica, _ik, ica_der) = cert("ICA", (2000, 1, 1), (2099, 1, 1));
    let (pu, pr, pu_der) = cert("Peer", (2000, 1, 1), (2099, 1, 1));

    let json = manifest(
        &dev,
        &ica,
        &[(pu, pr, vec![1; 8], vec![2; 8])],
        Some(vec![9, 9, 9]),
    );
    let store = CertificateStore::from_manifest_str(&json).unwrap();
    let bundle = store.active_bundle();

    assert_eq!(bundle.peer_cert_der, pu_der);
    assert_eq!(bundle.device_cert_der, dev_der);
    assert_eq!(bundle.intermediate_certs_der, vec![ica_der]);
    assert!(bundle.is_valid());
    assert_eq!(bundle.crl.as_deref(), Some(&[9, 9, 9][..]));
}

#[test]
fn auth_response_selects_signature_by_hash_and_honors_crl() {
    let (dev, _dk, dev_der) = cert("Device", (2000, 1, 1), (2099, 1, 1));
    let (ica, _ik, ica_der) = cert("ICA", (2000, 1, 1), (2099, 1, 1));
    let (pu, pr, _pu_der) = cert("Peer", (2000, 1, 1), (2099, 1, 1));
    let sig1 = vec![0xAA; 8];
    let sig256 = vec![0xBB; 8];

    let json = manifest(
        &dev,
        &ica,
        &[(pu, pr, sig1.clone(), sig256.clone())],
        Some(vec![9, 9, 9]),
    );
    let store = CertificateStore::from_manifest_str(&json).unwrap();
    let bundle = store.active_bundle();

    // SHA-1 → sig_sha1, bundle CRL used when no override.
    let resp =
        DeviceAuthMessage::decode(&build_auth_response(bundle, HashAlgorithm::Sha1, None)[..])
            .unwrap()
            .response
            .unwrap();
    assert_eq!(resp.signature, sig1);
    assert_eq!(resp.client_auth_certificate, dev_der);
    assert_eq!(resp.intermediate_certificate, vec![ica_der]);
    assert_eq!(resp.hash_algorithm, Some(HashAlgorithm::Sha1 as i32));
    assert_eq!(
        resp.signature_algorithm,
        Some(SignatureAlgorithm::RsassaPkcs1v15 as i32)
    );
    assert_eq!(resp.crl.as_deref(), Some(&[9, 9, 9][..]));

    // SHA-256 → sig_sha256, explicit CRL overrides bundle CRL.
    let resp = DeviceAuthMessage::decode(
        &build_auth_response(bundle, HashAlgorithm::Sha256, Some(&[7, 7]))[..],
    )
    .unwrap()
    .response
    .unwrap();
    assert_eq!(resp.signature, sig256);
    assert_eq!(resp.hash_algorithm, Some(HashAlgorithm::Sha256 as i32));
    assert_eq!(resp.crl.as_deref(), Some(&[7, 7][..]));
}

#[test]
fn auth_error_message_roundtrips() {
    let bytes = build_auth_error(AuthErrorType::SignatureAlgorithmUnavailable);
    let msg = DeviceAuthMessage::decode(&bytes[..]).unwrap();
    assert!(msg.response.is_none());
    assert_eq!(
        msg.error.unwrap().error_type,
        AuthErrorType::SignatureAlgorithmUnavailable as i32
    );
}

#[test]
fn cert_digest_md5_is_stable_hex() {
    let (dev, _dk, _) = cert("Device", (2000, 1, 1), (2099, 1, 1));
    let (ica, _ik, _) = cert("ICA", (2000, 1, 1), (2099, 1, 1));
    let (pu, pr, _) = cert("Peer", (2000, 1, 1), (2099, 1, 1));
    let json = manifest(&dev, &ica, &[(pu, pr, vec![1], vec![2])], None);
    let store = CertificateStore::from_manifest_str(&json).unwrap();
    let digest = store.active_bundle().cert_digest_md5();
    assert_eq!(digest.len(), 32);
    assert!(digest.chars().all(|c| c.is_ascii_hexdigit()));
    assert_eq!(digest, store.active_bundle().cert_digest_md5());
}

#[test]
fn manifest_without_currently_valid_cert_is_rejected() {
    let (dev, _dk, _) = cert("Device", (2000, 1, 1), (2099, 1, 1));
    let (ica, _ik, _) = cert("ICA", (2000, 1, 1), (2099, 1, 1));
    let (pu, pr, _) = cert("Expired", (2000, 1, 1), (2001, 1, 1));
    let json = manifest(&dev, &ica, &[(pu, pr, vec![1], vec![2])], None);
    assert!(matches!(
        CertificateStore::from_manifest_str(&json),
        Err(SecurityError::NoValidCert)
    ));
}

#[test]
fn rotates_to_next_valid_certificate_when_active_expires() {
    let (dev, _dk, _) = cert("Device", (2000, 1, 1), (2099, 1, 1));
    let (ica, _ik, _) = cert("ICA", (2000, 1, 1), (2099, 1, 1));
    let (pu_a, pr_a, der_a) = cert("A", (2000, 1, 1), (2030, 1, 1));
    let (pu_b, pr_b, der_b) = cert("B", (2030, 1, 1), (2040, 1, 1));

    let json = manifest(
        &dev,
        &ica,
        &[
            (pu_a, pr_a, vec![1], vec![2]),
            (pu_b, pr_b, vec![3], vec![4]),
        ],
        None,
    );
    let mut store = CertificateStore::from_manifest_str(&json).unwrap();

    assert_eq!(store.active_bundle().peer_cert_der, der_a);
    // Still within A's window → no rotation.
    assert!(store.rotate_if_needed(unix(2027, 1, 1)).unwrap().is_none());
    // Past A's expiry → rotate to B.
    let rotated = store.rotate_if_needed(unix(2035, 1, 1)).unwrap().unwrap();
    assert_eq!(rotated.peer_cert_der, der_b);
    assert_eq!(store.active_bundle().peer_cert_der, der_b);
}

#[test]
fn tls_server_config_builds_and_resolver_hot_reloads() {
    let (dev, _dk, _) = cert("Device", (2000, 1, 1), (2099, 1, 1));
    let (ica, _ik, _) = cert("ICA", (2000, 1, 1), (2099, 1, 1));
    let (pu, pr, _) = cert("Peer", (2000, 1, 1), (2099, 1, 1));
    let json = manifest(&dev, &ica, &[(pu, pr, vec![1], vec![2])], None);
    let store = CertificateStore::from_manifest_str(&json).unwrap();
    let bundle = store.active_bundle();

    let resolver = CertResolver::new(bundle).unwrap();
    let _config = server_config(resolver.clone()).expect("server config builds");
    // Hot-reload with the same bundle succeeds (exercises key reload path).
    resolver.update(bundle).unwrap();
}
