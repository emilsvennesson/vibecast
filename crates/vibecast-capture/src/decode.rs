//! Best-effort decoding of Cast message payloads for the capture log.
//!
//! STRING payloads are parsed as JSON when possible (so the log carries
//! structured data, not escaped strings); the binary `deviceauth` payload is
//! decoded into a non-sensitive summary (lengths, algorithms), never raw key
//! or signature bytes.

use prost::Message as _;
use serde_json::{json, Value};

use vibecast_proto::cast_channel::cast_message::PayloadType;
use vibecast_proto::{CastMessage, DeviceAuthMessage, HashAlgorithm, SignatureAlgorithm};

/// Transport-level Cast namespace for device authentication.
pub const DEVICE_AUTH: &str = "urn:x-cast:com.google.cast.tp.deviceauth";

/// `"string"` or `"binary"` for the log record.
#[must_use]
pub fn payload_type_name(msg: &CastMessage) -> &'static str {
    if msg.payload_type == PayloadType::Binary as i32 {
        "binary"
    } else {
        "string"
    }
}

/// Decode a message payload to a JSON value for logging.
#[must_use]
pub fn payload_value(msg: &CastMessage) -> Value {
    if msg.payload_type == PayloadType::Binary as i32 {
        let bytes = msg.payload_binary.as_deref().unwrap_or_default();
        if msg.namespace == DEVICE_AUTH {
            return decode_device_auth(bytes);
        }
        return json!({ "_binary_len": bytes.len() });
    }

    match &msg.payload_utf8 {
        Some(text) => {
            serde_json::from_str::<Value>(text).unwrap_or_else(|_| Value::from(text.clone()))
        }
        None => Value::Null,
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

#[cfg(test)]
mod tests {
    use super::*;
    use vibecast_proto::cast_channel::cast_message::ProtocolVersion;
    use vibecast_proto::AuthChallenge;

    fn binary(namespace: &str, payload: Vec<u8>) -> CastMessage {
        CastMessage {
            protocol_version: ProtocolVersion::Castv210 as i32,
            source_id: "s".into(),
            destination_id: "d".into(),
            namespace: namespace.into(),
            payload_type: PayloadType::Binary as i32,
            payload_utf8: None,
            payload_binary: Some(payload),
        }
    }

    fn string(payload: &str) -> CastMessage {
        CastMessage {
            protocol_version: ProtocolVersion::Castv210 as i32,
            source_id: "s".into(),
            destination_id: "d".into(),
            namespace: "urn:x-cast:com.google.cast.receiver".into(),
            payload_type: PayloadType::String as i32,
            payload_utf8: Some(payload.into()),
            payload_binary: None,
        }
    }

    #[test]
    fn parses_json_string_payload() {
        let v = payload_value(&string(r#"{"type":"LAUNCH","appId":"X"}"#));
        assert_eq!(v["type"], "LAUNCH");
        assert_eq!(v["appId"], "X");
    }

    #[test]
    fn keeps_non_json_string_verbatim() {
        assert_eq!(payload_value(&string("not json")), Value::from("not json"));
    }

    #[test]
    fn decodes_device_auth_challenge_summary() {
        let payload = DeviceAuthMessage {
            challenge: Some(AuthChallenge {
                signature_algorithm: Some(SignatureAlgorithm::RsassaPkcs1v15 as i32),
                sender_nonce: Some(vec![0u8; 16]),
                hash_algorithm: Some(HashAlgorithm::Sha256 as i32),
            }),
            response: None,
            error: None,
        }
        .encode_to_vec();

        let v = payload_value(&binary(DEVICE_AUTH, payload));
        assert_eq!(v["_decoded"], "DeviceAuthMessage.challenge");
        assert_eq!(v["hash_algorithm"], "SHA256");
        assert_eq!(v["signature_algorithm"], "RSASSA_PKCS1v15");
        assert_eq!(v["sender_nonce_len"], 16);
    }

    #[test]
    fn non_deviceauth_binary_is_length_only() {
        let v = payload_value(&binary("urn:x-cast:com.google.cast.media", vec![1, 2, 3]));
        assert_eq!(v["_binary_len"], 3);
    }
}
