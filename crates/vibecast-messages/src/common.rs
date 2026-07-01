//! Shared enums and reusable sub-models for Cast protocol messages.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Media stream type used in LOAD requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum StreamType {
    /// On-demand content.
    #[default]
    #[serde(rename = "BUFFERED")]
    Buffered,
    /// Live content.
    #[serde(rename = "LIVE")]
    Live,
    /// Unspecified.
    #[serde(rename = "NONE")]
    None,
}

/// Playback state reported in MEDIA_STATUS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PlayerState {
    /// No media loaded / stopped.
    #[default]
    #[serde(rename = "IDLE")]
    Idle,
    /// Actively playing.
    #[serde(rename = "PLAYING")]
    Playing,
    /// Paused.
    #[serde(rename = "PAUSED")]
    Paused,
    /// Buffering.
    #[serde(rename = "BUFFERING")]
    Buffering,
}

/// Reason the player entered IDLE.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IdleReason {
    /// Playback cancelled by the sender.
    #[serde(rename = "CANCELLED")]
    Cancelled,
    /// Interrupted by another load.
    #[serde(rename = "INTERRUPTED")]
    Interrupted,
    /// Reached end of content.
    #[serde(rename = "FINISHED")]
    Finished,
    /// Stopped due to an error.
    #[serde(rename = "ERROR")]
    Error,
}

/// Media category reported in `MediaInfo`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MediaCategory {
    /// Video content.
    #[serde(rename = "VIDEO")]
    Video,
    /// Audio content.
    #[serde(rename = "AUDIO")]
    Audio,
    /// Still image.
    #[serde(rename = "IMAGE")]
    Image,
}

/// Audio volume state (full form used in RECEIVER_STATUS).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Volume {
    /// Level in `[0.0, 1.0]`.
    pub level: f64,
    /// Whether muted.
    pub muted: bool,
    /// Volume control type (e.g. "attenuation").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_type: Option<String>,
    /// Volume step granularity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_interval: Option<f64>,
}

impl Default for Volume {
    fn default() -> Self {
        Self {
            level: 1.0,
            muted: false,
            control_type: None,
            step_interval: None,
        }
    }
}

impl Volume {
    /// Apply only the explicitly-provided fields from a SET_VOLUME update,
    /// leaving omitted fields unchanged (mirrors Pydantic `model_fields_set`).
    pub fn apply_update(&mut self, update: &VolumeUpdate) {
        if let Some(level) = update.level {
            self.level = level;
        }
        if let Some(muted) = update.muted {
            self.muted = muted;
        }
        if let Some(control_type) = &update.control_type {
            self.control_type = Some(control_type.clone());
        }
        if let Some(step) = update.step_interval {
            self.step_interval = Some(step);
        }
    }
}

/// Partial volume from a SET_VOLUME request; only present fields are applied.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VolumeUpdate {
    /// Requested level, if provided.
    pub level: Option<f64>,
    /// Requested mute state, if provided.
    pub muted: Option<bool>,
    /// Requested control type, if provided.
    pub control_type: Option<String>,
    /// Requested step interval, if provided.
    pub step_interval: Option<f64>,
}

/// A namespace entry in an application status block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CastNamespace {
    /// The namespace URI.
    pub name: String,
}

/// Status of a running Cast application.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationStatus {
    /// Cast application id.
    pub app_id: String,
    /// Human-readable name.
    pub display_name: String,
    /// Session id.
    pub session_id: String,
    /// Transport id for the session.
    pub transport_id: String,
    /// Status text.
    #[serde(default)]
    pub status_text: String,
    /// Namespaces the app handles.
    #[serde(default)]
    pub namespaces: Vec<CastNamespace>,
    /// Whether this is the idle screen app.
    #[serde(default)]
    pub is_idle_screen: bool,
    /// App type (e.g. "WEB").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_type: Option<String>,
    /// Icon URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon_url: Option<String>,
    /// Whether launched from cloud.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launched_from_cloud: Option<bool>,
    /// Whether a sender is connected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_connected: Option<bool>,
    /// Universal app id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub universal_app_id: Option<String>,
}

/// Top-level receiver status in RECEIVER_STATUS responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceiverStatus {
    /// Running applications.
    #[serde(default)]
    pub applications: Vec<ApplicationStatus>,
    /// Current volume.
    #[serde(default)]
    pub volume: Volume,
    /// Whether the input is active.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_active_input: Option<bool>,
    /// Whether the device is in standby.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_stand_by: Option<bool>,
}

/// An image reference within media metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MediaImage {
    /// Image URL.
    pub url: String,
    /// Optional height in pixels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    /// Optional width in pixels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
}

/// Metadata for a media item.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaMetadata {
    /// Metadata schema selector (0 = generic, 1 = movie, ...).
    #[serde(default)]
    pub metadata_type: u8,
    /// Title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Subtitle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<String>,
    /// Series title (TV).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub series_title: Option<String>,
    /// Season number (TV).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub season: Option<u32>,
    /// Episode number (TV).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub episode: Option<u32>,
    /// Associated images.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<MediaImage>,
}

/// Description of a media item in LOAD / MEDIA_STATUS messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaInfo {
    /// Logical content identifier (e.g. a content page URL).
    pub content_id: String,
    /// MIME type.
    #[serde(default)]
    pub content_type: String,
    /// Stream type.
    #[serde(default)]
    pub stream_type: StreamType,
    /// Optional metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<MediaMetadata>,
    /// Optional duration in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<f64>,
    /// App-specific data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_data: Option<Value>,
    /// Resolved playback manifest URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_url: Option<String>,
    /// Media category.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_category: Option<MediaCategory>,
}
