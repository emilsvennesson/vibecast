//! CastV2 protobuf types and length-prefixed message framing.
//!
//! The protobuf types are generated at build time from `proto/cast_channel.proto`
//! by `protox` + `prost` (see `build.rs`). Framing helpers live in `framing`.

#![forbid(unsafe_code)]

/// Generated CastV2 protobuf types (`package cast_channel`).
pub mod cast_channel {
    include!(concat!(env!("OUT_DIR"), "/cast_channel.rs"));
}

mod framing;

pub use cast_channel::{
    AuthChallenge, AuthError, AuthResponse, CastMessage, DeviceAuthMessage, HashAlgorithm,
    SignatureAlgorithm,
};
pub use framing::{decode_frame, encode_frame, CastCodec, FramingError, MAX_MESSAGE_SIZE};
