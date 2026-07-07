//! Owned data types exchanged between apps and the coordinator.

use serde_json::Value;
use vibecast_messages::{IdleReason, MediaImage, PlayerState, StreamType};

/// Supported DRM key systems.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrmSystem {
    /// Google Widevine (`com.widevine.alpha`).
    Widevine,
    /// Microsoft PlayReady (`com.microsoft.playready`).
    PlayReady,
    /// W3C ClearKey (`org.w3.clearkey`).
    ClearKey,
    /// Apple FairPlay Streaming (`com.apple.fps`).
    FairPlay,
}

/// DRM configuration for a protected stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrmInfo {
    /// Key system.
    pub system: DrmSystem,
    /// License acquisition URL.
    pub license_url: String,
    /// Extra headers required for license acquisition.
    pub headers: std::collections::HashMap<String, String>,
}

impl DrmInfo {
    /// Build DRM info with no extra headers.
    #[must_use]
    pub fn new(system: DrmSystem, license_url: impl Into<String>) -> Self {
        Self {
            system,
            license_url: license_url.into(),
            headers: std::collections::HashMap::new(),
        }
    }
}

/// A single playable stream candidate with optional DRM.
#[derive(Debug, Clone, PartialEq)]
pub struct PlaybackStream {
    /// Manifest / media URL.
    pub url: String,
    /// MIME type.
    pub content_type: String,
    /// DRM configuration, if the stream is protected.
    pub drm: Option<DrmInfo>,
}

/// Canonical media description returned by an app's `resolve_media`.
#[derive(Debug, Clone, PartialEq)]
pub struct PlaybackMedia {
    /// Owning session id.
    pub session_id: String,
    /// Stream candidates in preference order.
    pub streams: Vec<PlaybackStream>,
    /// On-demand vs live.
    pub stream_type: StreamType,
    /// Original content identifier from the LOAD request.
    pub content_id: Option<String>,
    /// Display title.
    pub title: Option<String>,
    /// Display subtitle.
    pub subtitle: Option<String>,
    /// Poster / artwork images.
    pub images: Vec<MediaImage>,
    /// Duration in seconds, if known.
    pub duration: Option<f64>,
    /// Whether playback starts automatically.
    pub autoplay: bool,
    /// Resume position in seconds.
    pub start_time: f64,
    /// App-specific data echoed to senders (`None` = omit).
    pub custom_data: Option<Value>,
}

impl PlaybackMedia {
    /// Build media with the common defaults (autoplay, no metadata).
    #[must_use]
    pub fn new(
        session_id: impl Into<String>,
        streams: Vec<PlaybackStream>,
        stream_type: StreamType,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            streams,
            stream_type,
            content_id: None,
            title: None,
            subtitle: None,
            images: Vec::new(),
            duration: None,
            autoplay: true,
            start_time: 0.0,
            custom_data: None,
        }
    }
}

/// Canonical playback state reported to apps via `on_playback_update`.
#[derive(Debug, Clone, PartialEq)]
pub struct PlaybackState {
    /// Playback state.
    pub player_state: PlayerState,
    /// Current position in seconds.
    pub current_time: f64,
    /// Total duration in seconds, if known.
    pub duration: Option<f64>,
    /// Reason for entering IDLE, if applicable.
    pub idle_reason: Option<IdleReason>,
}

/// Credentials supplied with a `LAUNCH` request.
#[derive(Debug, Clone, Default)]
pub struct LaunchCredentials {
    /// Opaque credentials blob.
    pub credentials: Option<String>,
    /// Credentials type discriminator.
    pub credentials_type: Option<String>,
}
