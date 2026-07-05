//! Cast protocol JSON message models.
//!
//! Typed serde structs and tagged enums for the CastV2 JSON namespaces. Most
//! messages use camelCase on the wire (`rename_all = "camelCase"`); the setup
//! namespace uses snake_case. Deserialization is lenient — unknown wire fields
//! are ignored — and serialization omits `None` optionals via
//! `skip_serializing_if`.

#![forbid(unsafe_code)]

pub mod common;
pub mod connection;
pub mod discovery;
pub mod media;
pub mod multizone;
pub mod patch;
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
pub use media::{
    media_command, ExtendedStatus, LoadFailedResponse, LoadRequest, MediaGetStatusRequest,
    MediaInvalidRequestResponse, MediaRequest, MediaSetVolumeRequest, MediaStatus,
    MediaStatusResponse, MediaStopRequest, PauseRequest, PlayRequest, QueueGetItemIdsRequest,
    QueueItemIdsResponse, QueueLoadRequest, RepeatMode, SeekRequest,
};
pub use multizone::{MultizoneGetStatusRequest, MultizoneStatus, MultizoneStatusResponse};
pub use patch::Patch;
pub use receiver::{
    AppAvailabilityResponse, GetAppAvailabilityRequest, GetStatusRequest, InvalidRequestResponse,
    LaunchErrorResponse, LaunchRequest, ReceiverRequest, ReceiverStatusResponse, SetVolumeRequest,
    StopRequest,
};
pub use setup::{SetupData, SetupDeviceInfo, SetupRequest, SetupResponse};
pub use util::extract_request_id;
