//! Build the binary `DeviceAuthMessage` sent on the deviceauth namespace.
//!
//! The signature is *not* computed at runtime — it is selected from the bundle's
//! pre-computed values based on the hash algorithm the sender requested.

use prost::Message;

use vibecast_proto::cast_channel::auth_error::ErrorType;
use vibecast_proto::{
    AuthError, AuthResponse, DeviceAuthMessage, HashAlgorithm, SignatureAlgorithm,
};

use crate::bundle::CertificateBundle;

/// Build a serialized `DeviceAuthMessage` carrying an `AuthResponse`.
///
/// `crl` overrides the bundle's embedded CRL when provided.
#[must_use]
pub fn build_auth_response(
    bundle: &CertificateBundle,
    hash_algorithm: HashAlgorithm,
    crl: Option<&[u8]>,
) -> Vec<u8> {
    let signature = match hash_algorithm {
        HashAlgorithm::Sha1 => bundle.signature_sha1.clone(),
        HashAlgorithm::Sha256 => bundle.signature_sha256.clone(),
    };

    let response = AuthResponse {
        signature,
        client_auth_certificate: bundle.device_cert_der.clone(),
        intermediate_certificate: bundle.intermediate_certs_der.clone(),
        signature_algorithm: Some(SignatureAlgorithm::RsassaPkcs1v15 as i32),
        sender_nonce: None,
        hash_algorithm: Some(hash_algorithm as i32),
        crl: crl.map(<[u8]>::to_vec).or_else(|| bundle.crl.clone()),
    };

    DeviceAuthMessage {
        challenge: None,
        response: Some(response),
        error: None,
    }
    .encode_to_vec()
}

/// Build a serialized `DeviceAuthMessage` carrying an `AuthError`.
#[must_use]
pub fn build_auth_error(error_type: ErrorType) -> Vec<u8> {
    DeviceAuthMessage {
        challenge: None,
        response: None,
        error: Some(AuthError {
            error_type: error_type as i32,
        }),
    }
    .encode_to_vec()
}
