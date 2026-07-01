//! Length-prefixed protobuf framing for the CastV2 protocol.
//!
//! Every Cast message on the wire is framed as:
//!
//! ```text
//! ┌──────────────────┬─────────────────────────────┐
//! │ 4 bytes (BE u32) │ N bytes (protobuf payload)   │
//! │ payload length   │ serialized CastMessage       │
//! └──────────────────┴─────────────────────────────┘
//! ```
//!
//! [`CastCodec`] implements [`tokio_util::codec`] `Encoder`/`Decoder` for
//! streaming use over a TLS connection; [`encode_frame`]/[`decode_frame`] are
//! convenience helpers for single, fully-buffered frames (tests, golden files).

use bytes::{Buf, BufMut, BytesMut};
use prost::Message;
use tokio_util::codec::{Decoder, Encoder};

use crate::cast_channel::CastMessage;

/// Maximum allowed message size (64 KiB). Cast messages are a few KB at most;
/// this bound protects against malformed or hostile streams.
pub const MAX_MESSAGE_SIZE: usize = 64 * 1024;

/// Size of the big-endian `u32` length prefix.
const PREFIX_LEN: usize = 4;

/// Protocol-level framing errors.
#[derive(Debug, thiserror::Error)]
pub enum FramingError {
    /// A frame declared a zero-byte payload.
    #[error("received zero-length message")]
    ZeroLength,
    /// A frame declared (or a message serialized to) more than [`MAX_MESSAGE_SIZE`].
    #[error("message too large: {size} bytes (max {MAX_MESSAGE_SIZE})")]
    TooLarge {
        /// The offending size in bytes.
        size: usize,
    },
    /// The payload was not a valid `CastMessage`.
    #[error("malformed protobuf payload ({size} bytes)")]
    Malformed {
        /// The payload length that failed to decode.
        size: usize,
        /// The underlying prost decode error.
        #[source]
        source: prost::DecodeError,
    },
    /// A single-frame helper was given fewer bytes than the frame declares.
    #[error("truncated frame")]
    Truncated,
    /// Transport I/O failure (surfaced by the streaming codec).
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Streaming codec for length-prefixed `CastMessage`s.
#[derive(Debug, Default, Clone, Copy)]
pub struct CastCodec;

impl Decoder for CastCodec {
    type Item = CastMessage;
    type Error = FramingError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        if src.len() < PREFIX_LEN {
            return Ok(None);
        }

        let mut prefix = [0u8; PREFIX_LEN];
        prefix.copy_from_slice(&src[..PREFIX_LEN]);
        let length = u32::from_be_bytes(prefix) as usize;

        if length == 0 {
            return Err(FramingError::ZeroLength);
        }
        if length > MAX_MESSAGE_SIZE {
            return Err(FramingError::TooLarge { size: length });
        }
        if src.len() < PREFIX_LEN + length {
            // Reserve so the transport can fill the rest of the frame in one go.
            src.reserve(PREFIX_LEN + length - src.len());
            return Ok(None);
        }

        src.advance(PREFIX_LEN);
        let payload = src.split_to(length);
        let message =
            CastMessage::decode(&payload[..]).map_err(|source| FramingError::Malformed {
                size: length,
                source,
            })?;
        Ok(Some(message))
    }
}

impl Encoder<CastMessage> for CastCodec {
    type Error = FramingError;

    fn encode(&mut self, item: CastMessage, dst: &mut BytesMut) -> Result<(), Self::Error> {
        put_frame(&item, dst)
    }
}

impl Encoder<&CastMessage> for CastCodec {
    type Error = FramingError;

    fn encode(&mut self, item: &CastMessage, dst: &mut BytesMut) -> Result<(), Self::Error> {
        put_frame(item, dst)
    }
}

fn put_frame(msg: &CastMessage, dst: &mut BytesMut) -> Result<(), FramingError> {
    let size = msg.encoded_len();
    if size > MAX_MESSAGE_SIZE {
        return Err(FramingError::TooLarge { size });
    }
    dst.reserve(PREFIX_LEN + size);
    dst.put_u32(size as u32);
    // Infallible: we reserved exactly enough capacity above.
    msg.encode(dst)
        .expect("capacity reserved for encoded message");
    Ok(())
}

/// Serialize `msg` into a single length-prefixed frame.
pub fn encode_frame(msg: &CastMessage) -> Result<Vec<u8>, FramingError> {
    let mut buf = BytesMut::new();
    put_frame(msg, &mut buf)?;
    Ok(buf.to_vec())
}

/// Decode a single length-prefixed frame from a fully-buffered slice.
///
/// Trailing bytes after the first frame are ignored.
pub fn decode_frame(frame: &[u8]) -> Result<CastMessage, FramingError> {
    let mut buf = BytesMut::from(frame);
    match CastCodec.decode(&mut buf)? {
        Some(msg) => Ok(msg),
        None => Err(FramingError::Truncated),
    }
}
