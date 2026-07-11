//! UniFFI facade exposing vibecast's reusable Google Cast building blocks.
//!
//! This crate is a thin, language-neutral view over the *extractable* Cast
//! implementation layer (`vibecast-proto`, `vibecast-security`,
//! `vibecast-discovery`): device authentication, length-prefixed framing,
//! payload decoding, and mDNS advertisement. It deliberately depends on nothing
//! above that layer (no platform/receiver/apps/bridge), so it can move with the
//! Cast layer if that is ever split into a standalone crate.
//!
//! Consumers (e.g. the `tools/capture/` Python dev tool) assemble these
//! primitives into higher-level tools such as a Cast MITM proxy. There is no
//! transport or proxy logic here — only building blocks, framed in generic
//! Google Cast vocabulary.
//!
//! `#![forbid(unsafe_code)]` is intentionally **not** applied: the whole point
//! of the crate is the FFI boundary, and `uniffi::setup_scaffolding!()` emits
//! `unsafe extern "C"` scaffolding. No hand-written `unsafe` appears here;
//! `#![deny(unsafe_code)]` still forbids that (UniFFI's generated code carries
//! its own `#[allow(unsafe_code)]`).
#![deny(unsafe_code)]

use std::sync::{Arc, Mutex, PoisonError};

use prost::Message as _;
use serde_json::{json, Value};

use vibecast_discovery::{CastAdvertisement, MdnsResponder};
use vibecast_proto::cast_channel::cast_message::PayloadType as ProtoPayloadType;
use vibecast_proto::{
    decode_frame, encode_frame, CastMessage as ProtoCastMessage, DeviceAuthMessage, FramingError,
    HashAlgorithm, SignatureAlgorithm,
};
use vibecast_security::{
    build_auth_error, build_auth_response, AuthErrorType, CertificateBundle, CertificateStore,
};

uniffi::setup_scaffolding!();

/// Transport-level Cast namespace for device authentication.
const DEVICE_AUTH: &str = "urn:x-cast:com.google.cast.tp.deviceauth";

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Failures surfaced across the FFI boundary as typed exceptions.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum PrimitivesError {
    /// Loading or selecting a certificate from the manifest failed.
    #[error("certificate manifest: {reason}")]
    Manifest {
        /// Human-readable cause.
        reason: String,
    },
    /// A frame could not be parsed or serialized.
    #[error("framing: {reason}")]
    Framing {
        /// Human-readable cause.
        reason: String,
    },
    /// Starting the mDNS responder failed.
    #[error("mdns: {reason}")]
    Mdns {
        /// Human-readable cause.
        reason: String,
    },
}

// ---------------------------------------------------------------------------
// Cast message model
// ---------------------------------------------------------------------------

/// Whether a Cast message payload is a UTF-8 string or opaque binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum PayloadType {
    /// A UTF-8 string payload (`payload_utf8`).
    String,
    /// A binary payload (`payload_binary`).
    Binary,
}

/// A single CastV2 message, mirroring the protobuf `CastMessage`.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct CastMessage {
    /// Protocol version (echo the value received when replying).
    pub protocol_version: i32,
    /// Source sender/receiver id.
    pub source_id: String,
    /// Destination sender/receiver id.
    pub destination_id: String,
    /// Message namespace (`urn:x-cast:...`).
    pub namespace: String,
    /// Which payload field is populated.
    pub payload_type: PayloadType,
    /// The string payload, when `payload_type` is `String`.
    pub payload_utf8: Option<String>,
    /// The binary payload, when `payload_type` is `Binary`.
    pub payload_binary: Option<Vec<u8>>,
}

impl From<ProtoCastMessage> for CastMessage {
    fn from(m: ProtoCastMessage) -> Self {
        let payload_type = if m.payload_type == ProtoPayloadType::Binary as i32 {
            PayloadType::Binary
        } else {
            PayloadType::String
        };
        Self {
            protocol_version: m.protocol_version,
            source_id: m.source_id,
            destination_id: m.destination_id,
            namespace: m.namespace,
            payload_type,
            payload_utf8: m.payload_utf8,
            payload_binary: m.payload_binary,
        }
    }
}

impl CastMessage {
    fn into_proto(self) -> ProtoCastMessage {
        let payload_type = match self.payload_type {
            PayloadType::Binary => ProtoPayloadType::Binary as i32,
            PayloadType::String => ProtoPayloadType::String as i32,
        };
        ProtoCastMessage {
            protocol_version: self.protocol_version,
            source_id: self.source_id,
            destination_id: self.destination_id,
            namespace: self.namespace,
            payload_type,
            payload_utf8: self.payload_utf8,
            payload_binary: self.payload_binary,
        }
    }
}

/// The result of parsing a frame off the front of a buffer.
#[derive(Debug, Clone, uniffi::Record)]
pub struct ParsedFrame {
    /// The decoded message.
    pub message: CastMessage,
    /// Number of bytes consumed from the buffer (length prefix + payload).
    pub consumed: u32,
}

// ---------------------------------------------------------------------------
// Framing
// ---------------------------------------------------------------------------

/// Try to parse one length-prefixed `CastMessage` off the front of `buffer`.
///
/// Returns `Ok(None)` if the buffer does not yet hold a complete frame (the
/// caller should read more bytes and retry). On success, `consumed` tells the
/// caller how many leading bytes to drop before the next parse. Trailing bytes
/// after the first frame are left untouched.
#[uniffi::export]
pub fn try_parse_frame(buffer: Vec<u8>) -> Result<Option<ParsedFrame>, PrimitivesError> {
    match decode_frame(&buffer) {
        Ok(message) => {
            // `decode_frame` succeeded, so at least the 4-byte length prefix is
            // present; `consumed` = prefix + declared payload length.
            let payload_len =
                u32::from_be_bytes([buffer[0], buffer[1], buffer[2], buffer[3]]) as u64;
            let consumed = (payload_len + 4).min(u64::from(u32::MAX)) as u32;
            Ok(Some(ParsedFrame {
                message: message.into(),
                consumed,
            }))
        }
        Err(FramingError::Truncated) => Ok(None),
        Err(error) => Err(PrimitivesError::Framing {
            reason: error.to_string(),
        }),
    }
}

/// Serialize a `CastMessage` into a single length-prefixed frame.
#[uniffi::export]
pub fn serialize_frame(message: CastMessage) -> Result<Vec<u8>, PrimitivesError> {
    encode_frame(&message.into_proto()).map_err(|error| PrimitivesError::Framing {
        reason: error.to_string(),
    })
}

// ---------------------------------------------------------------------------
// Payload decoding (for logging / analysis)
// ---------------------------------------------------------------------------

/// Decode a message payload to a JSON string for logging.
///
/// STRING payloads are parsed as JSON when possible; the binary `deviceauth`
/// payload is decoded into a non-sensitive summary (lengths + algorithms) that
/// never includes key or signature bytes; other binary payloads are summarized
/// by length only.
#[uniffi::export]
pub fn decode_payload_json(message: CastMessage) -> String {
    payload_value(&message).to_string()
}

fn payload_value(msg: &CastMessage) -> Value {
    match msg.payload_type {
        PayloadType::Binary => {
            let bytes = msg.payload_binary.as_deref().unwrap_or_default();
            if msg.namespace == DEVICE_AUTH {
                decode_device_auth(bytes)
            } else {
                json!({ "_binary_len": bytes.len() })
            }
        }
        PayloadType::String => match &msg.payload_utf8 {
            Some(text) => {
                serde_json::from_str::<Value>(text).unwrap_or_else(|_| Value::from(text.clone()))
            }
            None => Value::Null,
        },
    }
}

fn hash_name(v: i32) -> Value {
    match HashAlgorithm::try_from(v) {
        Ok(HashAlgorithm::Sha1) => Value::from("SHA1"),
        Ok(HashAlgorithm::Sha256) => Value::from("SHA256"),
        Err(_) => Value::from(v),
    }
}

fn sig_name(v: i32) -> Value {
    match SignatureAlgorithm::try_from(v) {
        Ok(SignatureAlgorithm::RsassaPkcs1v15) => Value::from("RSASSA_PKCS1v15"),
        Ok(SignatureAlgorithm::RsassaPss) => Value::from("RSASSA_PSS"),
        Ok(SignatureAlgorithm::Unspecified) => Value::from("UNSPECIFIED"),
        Err(_) => Value::from(v),
    }
}

/// Summarize a `DeviceAuthMessage` without exposing key/signature bytes.
fn decode_device_auth(bytes: &[u8]) -> Value {
    let Ok(msg) = DeviceAuthMessage::decode(bytes) else {
        return json!({ "_decode_error": true, "_binary_len": bytes.len() });
    };

    if let Some(ch) = msg.challenge {
        return json!({
            "_decoded": "DeviceAuthMessage.challenge",
            "hash_algorithm": hash_name(ch.hash_algorithm.unwrap_or(HashAlgorithm::Sha1 as i32)),
            "signature_algorithm":
                sig_name(ch.signature_algorithm.unwrap_or(SignatureAlgorithm::RsassaPkcs1v15 as i32)),
            "sender_nonce_len": ch.sender_nonce.map(|n| n.len()),
        });
    }
    if let Some(resp) = msg.response {
        return json!({
            "_decoded": "DeviceAuthMessage.response",
            "signature_len": resp.signature.len(),
            "client_auth_certificate_len": resp.client_auth_certificate.len(),
            "intermediate_certificate_count": resp.intermediate_certificate.len(),
            "crl_len": resp.crl.as_ref().map(Vec::len),
            "hash_algorithm": resp.hash_algorithm.map(hash_name),
            "signature_algorithm": resp.signature_algorithm.map(sig_name),
        });
    }
    if let Some(err) = msg.error {
        return json!({ "_decoded": "DeviceAuthMessage.error", "error_type": err.error_type });
    }
    json!({ "_decoded": "DeviceAuthMessage", "_empty": true })
}

// ---------------------------------------------------------------------------
// Certificate bundle + device authentication
// ---------------------------------------------------------------------------

/// A loaded Cast certificate bundle: the harvested TLS/device-auth material a
/// proxy or receiver presents to senders.
#[derive(uniffi::Object)]
pub struct CertBundle {
    bundle: CertificateBundle,
}

#[uniffi::export]
impl CertBundle {
    /// Load the active bundle from a certificate manifest JSON file.
    #[uniffi::constructor]
    pub fn load(manifest_path: String) -> Result<Arc<Self>, PrimitivesError> {
        let store = CertificateStore::from_manifest_path(&manifest_path).map_err(|error| {
            PrimitivesError::Manifest {
                reason: error.to_string(),
            }
        })?;
        Ok(Arc::new(Self {
            bundle: store.active_bundle().clone(),
        }))
    }

    /// The TLS server certificate (PEM) to present to senders.
    #[must_use]
    pub fn peer_cert_pem(&self) -> Vec<u8> {
        self.bundle.peer_cert_pem.clone()
    }

    /// The TLS server private key (PEM).
    #[must_use]
    pub fn peer_key_pem(&self) -> Vec<u8> {
        self.bundle.peer_key_pem.clone()
    }

    /// MD5 hex digest of the peer certificate, for the mDNS `cd` field.
    #[must_use]
    pub fn cert_digest_md5(&self) -> String {
        self.bundle.cert_digest_md5()
    }

    /// Whether the bundle carries a CRL (included in auth responses).
    #[must_use]
    pub fn has_crl(&self) -> bool {
        self.bundle.crl.is_some()
    }

    /// Answer a sender's `deviceauth` challenge, producing the reply message.
    ///
    /// The response is signed with the bundle's *pre-computed* signature for the
    /// requested hash algorithm (no runtime signing); source/destination ids are
    /// swapped so it can be sent straight back to the sender. Unsupported
    /// signature/hash algorithms yield a `DeviceAuthMessage.error` reply.
    #[must_use]
    pub fn device_auth_reply(&self, challenge: CastMessage) -> CastMessage {
        let payload = challenge.payload_binary.as_deref().unwrap_or_default();
        let reply_payload = device_auth_payload(&self.bundle, payload);
        CastMessage {
            protocol_version: challenge.protocol_version,
            source_id: challenge.destination_id,
            destination_id: challenge.source_id,
            namespace: DEVICE_AUTH.to_owned(),
            payload_type: PayloadType::Binary,
            payload_utf8: None,
            payload_binary: Some(reply_payload),
        }
    }
}

/// Build the `deviceauth` reply payload for a challenge (proto2 defaults:
/// hash=SHA1, signature=RSASSA_PKCS1v15 when unset).
fn device_auth_payload(bundle: &CertificateBundle, payload: &[u8]) -> Vec<u8> {
    let (hash, sig) = match DeviceAuthMessage::decode(payload) {
        Ok(DeviceAuthMessage {
            challenge: Some(ch),
            ..
        }) => (
            ch.hash_algorithm.unwrap_or(HashAlgorithm::Sha1 as i32),
            ch.signature_algorithm
                .unwrap_or(SignatureAlgorithm::RsassaPkcs1v15 as i32),
        ),
        _ => (
            HashAlgorithm::Sha1 as i32,
            SignatureAlgorithm::RsassaPkcs1v15 as i32,
        ),
    };

    if sig != SignatureAlgorithm::RsassaPkcs1v15 as i32 {
        return build_auth_error(AuthErrorType::SignatureAlgorithmUnavailable);
    }
    match HashAlgorithm::try_from(hash) {
        // `None` crl override => the bundle's embedded CRL is used.
        Ok(hash) => build_auth_response(bundle, hash, None),
        Err(_) => build_auth_error(AuthErrorType::SignatureAlgorithmUnavailable),
    }
}

// ---------------------------------------------------------------------------
// mDNS advertisement
// ---------------------------------------------------------------------------

/// A live `_googlecast._tcp` mDNS advertisement. Dropping it (or calling
/// [`stop`](CastAdvertiser::stop)) stops advertising.
#[derive(uniffi::Object)]
pub struct CastAdvertiser {
    responder: Mutex<Option<MdnsResponder>>,
}

#[uniffi::export]
impl CastAdvertiser {
    /// Start advertising a Cast device with the given identity + TXT digest.
    #[uniffi::constructor]
    pub fn start(
        friendly_name: String,
        model: String,
        device_id: String,
        port: u16,
        cert_digest: String,
    ) -> Result<Arc<Self>, PrimitivesError> {
        let advertisement =
            CastAdvertisement::new(&friendly_name, &model, &device_id, port, &cert_digest);
        let responder =
            MdnsResponder::start(&advertisement).map_err(|error| PrimitivesError::Mdns {
                reason: error.to_string(),
            })?;
        Ok(Arc::new(Self {
            responder: Mutex::new(Some(responder)),
        }))
    }

    /// Stop advertising. Idempotent.
    pub fn stop(&self) {
        if let Some(mut responder) = self
            .responder
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            responder.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use vibecast_proto::cast_channel::cast_message::ProtocolVersion;
    use vibecast_proto::{AuthChallenge, DeviceAuthMessage};

    fn test_bundle() -> CertificateBundle {
        CertificateBundle {
            peer_cert_pem: b"peer-cert-pem".to_vec(),
            peer_key_pem: b"peer-key-pem".to_vec(),
            peer_cert_der: vec![1, 2, 3, 4],
            device_cert_der: vec![10, 11, 12],
            intermediate_certs_der: vec![vec![20, 21]],
            signature_sha1: vec![0xA1; 8],
            signature_sha256: vec![0xA2; 8],
            not_valid_before: 0,
            not_valid_after: i64::MAX,
            crl: None,
        }
    }

    fn challenge_message(hash: HashAlgorithm) -> CastMessage {
        let payload = DeviceAuthMessage {
            challenge: Some(AuthChallenge {
                signature_algorithm: Some(SignatureAlgorithm::RsassaPkcs1v15 as i32),
                sender_nonce: None,
                hash_algorithm: Some(hash as i32),
            }),
            response: None,
            error: None,
        }
        .encode_to_vec();
        CastMessage {
            protocol_version: ProtocolVersion::Castv210 as i32,
            source_id: "sender-0".into(),
            destination_id: "receiver-0".into(),
            namespace: DEVICE_AUTH.into(),
            payload_type: PayloadType::Binary,
            payload_utf8: None,
            payload_binary: Some(payload),
        }
    }

    #[test]
    fn device_auth_reply_swaps_ids_and_signs_with_requested_hash() {
        let bundle = CertBundle {
            bundle: test_bundle(),
        };
        let reply = bundle.device_auth_reply(challenge_message(HashAlgorithm::Sha256));

        assert_eq!(reply.namespace, DEVICE_AUTH);
        assert_eq!(reply.source_id, "receiver-0");
        assert_eq!(reply.destination_id, "sender-0");
        assert_eq!(reply.payload_type, PayloadType::Binary);

        let decoded = DeviceAuthMessage::decode(reply.payload_binary.as_deref().unwrap())
            .unwrap()
            .response
            .expect("a response, not an error");
        // SHA-256 challenge -> the bundle's pre-computed SHA-256 signature.
        assert_eq!(decoded.signature, vec![0xA2; 8]);
    }

    #[test]
    fn device_auth_reply_errors_on_unsupported_signature() {
        let payload = DeviceAuthMessage {
            challenge: Some(AuthChallenge {
                signature_algorithm: Some(SignatureAlgorithm::RsassaPss as i32),
                sender_nonce: None,
                hash_algorithm: Some(HashAlgorithm::Sha256 as i32),
            }),
            response: None,
            error: None,
        }
        .encode_to_vec();
        let challenge = CastMessage {
            protocol_version: 0,
            source_id: "s".into(),
            destination_id: "d".into(),
            namespace: DEVICE_AUTH.into(),
            payload_type: PayloadType::Binary,
            payload_utf8: None,
            payload_binary: Some(payload),
        };
        let bundle = CertBundle {
            bundle: test_bundle(),
        };
        let reply = bundle.device_auth_reply(challenge);
        let decoded = DeviceAuthMessage::decode(reply.payload_binary.as_deref().unwrap()).unwrap();
        assert!(decoded.response.is_none());
        assert!(decoded.error.is_some());
    }

    #[test]
    fn frame_round_trips_and_reports_consumed() {
        let message = CastMessage {
            protocol_version: ProtocolVersion::Castv210 as i32,
            source_id: "sender-0".into(),
            destination_id: "receiver-0".into(),
            namespace: "urn:x-cast:com.google.cast.receiver".into(),
            payload_type: PayloadType::String,
            payload_utf8: Some(r#"{"type":"GET_STATUS"}"#.into()),
            payload_binary: None,
        };
        let frame = serialize_frame(message.clone()).unwrap();

        // A trailing byte must be left untouched (consumed excludes it).
        let mut buffer = frame.clone();
        buffer.push(0xFF);
        let parsed = try_parse_frame(buffer).unwrap().expect("a complete frame");
        assert_eq!(parsed.message, message);
        assert_eq!(parsed.consumed as usize, frame.len());
    }

    #[test]
    fn try_parse_frame_returns_none_when_incomplete() {
        let frame = serialize_frame(CastMessage {
            protocol_version: 0,
            source_id: "s".into(),
            destination_id: "d".into(),
            namespace: "urn:x-cast:com.google.cast.receiver".into(),
            payload_type: PayloadType::String,
            payload_utf8: Some("{}".into()),
            payload_binary: None,
        })
        .unwrap();
        // Only the length prefix + one payload byte: not a full frame yet.
        assert!(try_parse_frame(frame[..5].to_vec()).unwrap().is_none());
        // Fewer than the 4-byte prefix: also incomplete.
        assert!(try_parse_frame(vec![0, 0]).unwrap().is_none());
    }

    #[test]
    fn decode_payload_json_parses_string_and_summarizes_device_auth() {
        let string_msg = CastMessage {
            protocol_version: 0,
            source_id: "s".into(),
            destination_id: "d".into(),
            namespace: "urn:x-cast:com.google.cast.receiver".into(),
            payload_type: PayloadType::String,
            payload_utf8: Some(r#"{"type":"LAUNCH","appId":"X"}"#.into()),
            payload_binary: None,
        };
        let json: Value = serde_json::from_str(&decode_payload_json(string_msg)).unwrap();
        assert_eq!(json["type"], "LAUNCH");
        assert_eq!(json["appId"], "X");

        let auth_json: Value = serde_json::from_str(&decode_payload_json(challenge_message(
            HashAlgorithm::Sha256,
        )))
        .unwrap();
        assert_eq!(auth_json["_decoded"], "DeviceAuthMessage.challenge");
        assert_eq!(auth_json["hash_algorithm"], "SHA256");
    }
}
