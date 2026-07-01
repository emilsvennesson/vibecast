//! Cast protocol JSON message models.
//!
//! Mirrors the Python `_models` package. Most messages use camelCase on the
//! wire (`rename_all = "camelCase"`); the setup namespace uses snake_case.
//! Deserialization is lenient (unknown fields ignored); serialization omits
//! `None` optionals (matching Pydantic `exclude_none`).

#![forbid(unsafe_code)]

pub mod common;
pub mod connection;
pub mod discovery;
pub mod multizone;
pub mod receiver;
pub mod setup;

mod util;

#[cfg(test)]
mod tests;

pub use common::{
    ApplicationStatus, CastNamespace, IdleReason, MediaCategory, MediaImage, MediaInfo,
    MediaMetadata, PlayerState, ReceiverStatus, StreamType, Volume, VolumeUpdate,
};
pub use connection::{CloseRequest, ConnectRequest, ConnectionMessage, SenderInfo};
pub use discovery::{DeviceInfoResponse, GetDeviceInfoRequest};
pub use multizone::{MultizoneGetStatusRequest, MultizoneStatus, MultizoneStatusResponse};
pub use receiver::{
    AppAvailabilityResponse, GetAppAvailabilityRequest, GetStatusRequest, InvalidRequestResponse,
    LaunchErrorResponse, LaunchRequest, ReceiverRequest, ReceiverStatusResponse, SetVolumeRequest,
    StopRequest,
};
pub use setup::{SetupData, SetupDeviceInfo, SetupRequest, SetupResponse};
pub use util::extract_request_id;
