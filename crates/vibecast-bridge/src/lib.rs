//! Player bridge: the HTTP/WebSocket seam between the coordinator and external
//! renderers (browser / Kodi).
//!
//! Ports `vibecast._playback` and `vibecast.player`. The bridge serves the
//! embedded Shaka Player page, relays [`PlayerCommand`]s to renderers over a
//! WebSocket, forwards [`PlayerReport`]s from the primary renderer back to the
//! coordinator, and proxies DRM license and DASH/HLS manifest requests (with
//! normalization) on behalf of the active session.

#![forbid(unsafe_code)]

mod bridge;
pub mod headers;
pub mod manifest;
pub mod protocol;
pub mod proxy;
mod web;

pub use bridge::{PlayerBridge, Renderer};
pub use manifest::{
    default_manifest_content_type, infer_manifest_kind, manifest_route_suffix,
    normalize_manifest_bytes, ManifestKind,
};
pub use protocol::{
    DrmPayload, DrmSystem, PlaybackMediaPayload, PlaybackStreamPayload, PlayerCommand, PlayerReport,
};
pub use proxy::{
    LicenseHandler, LicenseRequest, LicenseResponse, ManifestHandler, ManifestProxyRequest,
    ManifestProxyResponse, ProxyError, ProxyResult,
};
pub use web::{PLAYER_HTML, PLAYER_JS};
