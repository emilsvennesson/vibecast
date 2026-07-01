//! Framing tests: round-trip, streaming (partial/coalesced reads), and error paths.

use bytes::{BufMut, BytesMut};
use proptest::prelude::*;
use tokio_util::codec::Decoder;

use vibecast_proto::cast_channel::cast_message::{PayloadType, ProtocolVersion};
use vibecast_proto::{
    decode_frame, encode_frame, CastCodec, CastMessage, FramingError, MAX_MESSAGE_SIZE,
};

fn string_message(payload: &str) -> CastMessage {
    CastMessage {
        protocol_version: ProtocolVersion::Castv210 as i32,
        source_id: "sender-0".into(),
        destination_id: "receiver-0".into(),
        namespace: "urn:x-cast:com.google.cast.tp.connection".into(),
        payload_type: PayloadType::String as i32,
        payload_utf8: Some(payload.into()),
        payload_binary: None,
    }
}

fn binary_message(payload: Vec<u8>) -> CastMessage {
    CastMessage {
        protocol_version: ProtocolVersion::Castv210 as i32,
        source_id: "a".into(),
        destination_id: "b".into(),
        namespace: "urn:x-cast:com.google.cast.media".into(),
        payload_type: PayloadType::Binary as i32,
        payload_utf8: None,
        payload_binary: Some(payload),
    }
}

#[test]
fn frame_prefix_is_be_length_of_payload() {
    let frame = encode_frame(&string_message("{\"type\":\"PING\"}")).unwrap();
    let declared = u32::from_be_bytes(frame[..4].try_into().unwrap()) as usize;
    assert_eq!(declared, frame.len() - 4);
}

#[test]
fn round_trip_string_and_binary() {
    let s = string_message("{\"type\":\"CONNECT\"}");
    assert_eq!(decode_frame(&encode_frame(&s).unwrap()).unwrap(), s);

    let b = binary_message(vec![0, 1, 2, 255, 254]);
    assert_eq!(decode_frame(&encode_frame(&b).unwrap()).unwrap(), b);
}

#[test]
fn codec_handles_partial_then_completed_frame() {
    let frame = encode_frame(&string_message("hello")).unwrap();
    let mut codec = CastCodec;
    let mut buf = BytesMut::new();

    // Fewer than 4 prefix bytes: not ready.
    buf.extend_from_slice(&frame[..3]);
    assert!(codec.decode(&mut buf).unwrap().is_none());

    // Prefix complete but payload short by one byte: still not ready.
    buf.extend_from_slice(&frame[3..frame.len() - 1]);
    assert!(codec.decode(&mut buf).unwrap().is_none());

    // Final byte arrives: full message decodes and buffer drains.
    buf.extend_from_slice(&frame[frame.len() - 1..]);
    assert_eq!(
        codec.decode(&mut buf).unwrap().unwrap(),
        string_message("hello")
    );
    assert!(codec.decode(&mut buf).unwrap().is_none());
}

#[test]
fn codec_decodes_multiple_coalesced_frames() {
    let one = string_message("one");
    let two = binary_message(vec![9, 8, 7]);
    let mut buf = BytesMut::new();
    buf.extend_from_slice(&encode_frame(&one).unwrap());
    buf.extend_from_slice(&encode_frame(&two).unwrap());

    let mut codec = CastCodec;
    assert_eq!(codec.decode(&mut buf).unwrap().unwrap(), one);
    assert_eq!(codec.decode(&mut buf).unwrap().unwrap(), two);
    assert!(codec.decode(&mut buf).unwrap().is_none());
}

#[test]
fn zero_length_frame_is_rejected() {
    let mut buf = BytesMut::from(&[0u8, 0, 0, 0][..]);
    assert!(matches!(
        CastCodec.decode(&mut buf),
        Err(FramingError::ZeroLength)
    ));
}

#[test]
fn oversized_declared_length_is_rejected() {
    let mut buf = BytesMut::new();
    buf.put_u32((MAX_MESSAGE_SIZE + 1) as u32);
    assert!(matches!(
        CastCodec.decode(&mut buf),
        Err(FramingError::TooLarge { size }) if size == MAX_MESSAGE_SIZE + 1
    ));
}

#[test]
fn oversized_message_fails_to_encode() {
    let big = string_message(&"x".repeat(MAX_MESSAGE_SIZE + 10));
    assert!(matches!(
        encode_frame(&big),
        Err(FramingError::TooLarge { .. })
    ));
}

#[test]
fn malformed_payload_is_rejected() {
    // Length prefix of 1, then a lone field-1 varint tag with no value byte.
    let mut buf = BytesMut::new();
    buf.put_u32(1);
    buf.put_u8(0x08);
    assert!(matches!(
        CastCodec.decode(&mut buf),
        Err(FramingError::Malformed { size: 1, .. })
    ));
}

#[test]
fn decode_frame_reports_truncation() {
    let frame = encode_frame(&string_message("truncate me")).unwrap();
    assert!(matches!(
        decode_frame(&frame[..frame.len() - 2]),
        Err(FramingError::Truncated)
    ));
}

proptest! {
    #[test]
    fn round_trip_arbitrary_string_message(
        src in ".*", dst in ".*", ns in ".*",
        payload in proptest::option::of(".*"),
    ) {
        let msg = CastMessage {
            protocol_version: ProtocolVersion::Castv210 as i32,
            source_id: src,
            destination_id: dst,
            namespace: ns,
            payload_type: PayloadType::String as i32,
            payload_utf8: payload,
            payload_binary: None,
        };
        let frame = encode_frame(&msg).unwrap();
        prop_assert_eq!(decode_frame(&frame).unwrap(), msg);
    }

    #[test]
    fn round_trip_arbitrary_binary_message(bin in proptest::collection::vec(any::<u8>(), 0..4096)) {
        let msg = binary_message(bin);
        let frame = encode_frame(&msg).unwrap();
        prop_assert_eq!(decode_frame(&frame).unwrap(), msg);
    }
}
