//! Helpers for constructing outbound [`CastMessage`]s.

use vibecast_proto::cast_channel::cast_message::{PayloadType, ProtocolVersion};
use vibecast_proto::CastMessage;

/// Build a STRING (JSON/text) Cast message with all required fields set.
#[must_use]
pub fn build_string(
    source_id: &str,
    dest_id: &str,
    namespace: &str,
    payload: String,
) -> CastMessage {
    CastMessage {
        protocol_version: ProtocolVersion::Castv210 as i32,
        source_id: source_id.to_owned(),
        destination_id: dest_id.to_owned(),
        namespace: namespace.to_owned(),
        payload_type: PayloadType::String as i32,
        payload_utf8: Some(payload),
        payload_binary: None,
    }
}

/// Build a BINARY Cast message with all required fields set.
#[must_use]
pub fn build_binary(
    source_id: &str,
    dest_id: &str,
    namespace: &str,
    payload: Vec<u8>,
) -> CastMessage {
    CastMessage {
        protocol_version: ProtocolVersion::Castv210 as i32,
        source_id: source_id.to_owned(),
        destination_id: dest_id.to_owned(),
        namespace: namespace.to_owned(),
        payload_type: PayloadType::Binary as i32,
        payload_utf8: None,
        payload_binary: Some(payload),
    }
}
