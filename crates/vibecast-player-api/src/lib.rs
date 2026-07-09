//! The neutral API between the generic Cast receiver runtime and a concrete
//! player implementation.
//!
//! The receiver core ([`vibecast-core`]) drives playback purely through the
//! seams defined here — it hands media to a [`Player`] and registers
//! DRM-license / manifest proxy handlers through a [`ProxyRegistrar`] — without
//! depending on any particular player (the bundled Shaka bridge, a native
//! player, or a test double). This crate carries:
//!
//! - the WebSocket wire protocol ([`PlayerCommand`] / [`PlayerReport`]),
//! - the [`Player`] command sink and [`ProxyRegistrar`] proxy seam,
//! - the proxy request/response contracts and typed [`RouteId`] selectors,
//! - manifest normalization and HTTP header filtering used by proxy handlers.

#![forbid(unsafe_code)]

pub mod headers;
pub mod manifest;
mod player;
pub mod protocol;
pub mod proxy;

pub use manifest::{
    default_manifest_content_type, infer_manifest_kind, manifest_route_suffix,
    normalize_manifest_bytes, ManifestKind,
};
pub use player::Player;
pub use protocol::{
    DrmPayload, DrmSystem, PlaybackMediaPayload, PlaybackStreamPayload, PlayerCommand, PlayerReport,
};
pub use proxy::{
    LicenseHandler, LicenseRequest, LicenseResponse, ManifestHandler, ManifestProxyRequest,
    ManifestProxyResponse, ProxyError, ProxyRegistrar, ProxyResult, RouteId, RouteIdParseError,
    RouteKind,
};
