//! WebSocket wire protocol between the bridge and external renderers.
//!
//! [`PlayerCommand`] flows bridge → renderer; [`PlayerReport`] flows renderer →
//! bridge. Both are `#[serde(tag = "type")]` enums with camelCase field names
//! on the wire.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use vibecast_messages::{IdleReason, MediaImage, PlayerState, StreamType};

/// Supported DRM key systems (EME key-system identifiers).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DrmSystem {
    /// Google Widevine.
    #[serde(rename = "com.widevine.alpha")]
    Widevine,
    /// W3C ClearKey.
    #[serde(rename = "org.w3.clearkey")]
    ClearKey,
}

/// DRM configuration attached to a stream on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DrmPayload {
    /// Key system.
    pub system: DrmSystem,
    /// License acquisition URL (rewritten to the bridge proxy by the coordinator).
    pub license_url: String,
    /// Extra headers the renderer should attach to license requests.
    #[serde(default)]
    pub headers: HashMap<String, String>,
}

/// One playable stream candidate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackStreamPayload {
    /// Manifest / media URL.
    pub url: String,
    /// MIME type.
    pub content_type: String,
    /// DRM configuration, if the stream is protected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drm: Option<DrmPayload>,
}

/// Media description carried by a `load` command.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackMediaPayload {
    /// Stream candidates in preference order.
    #[serde(default)]
    pub streams: Vec<PlaybackStreamPayload>,
    /// On-demand vs live.
    pub stream_type: StreamType,
    /// Display title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Display subtitle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<String>,
    /// Poster / artwork images.
    #[serde(default)]
    pub images: Vec<MediaImage>,
    /// Duration in seconds, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<f64>,
    /// Whether playback starts automatically.
    #[serde(default = "default_true")]
    pub autoplay: bool,
    /// Resume position in seconds.
    #[serde(default)]
    pub start_time: f64,
    /// App-specific opaque data.
    #[serde(default = "empty_object")]
    pub custom_data: Value,
}

impl Default for PlaybackMediaPayload {
    fn default() -> Self {
        Self {
            streams: Vec::new(),
            stream_type: StreamType::default(),
            title: None,
            subtitle: None,
            images: Vec::new(),
            duration: None,
            autoplay: true,
            start_time: 0.0,
            custom_data: empty_object(),
        }
    }
}

fn default_true() -> bool {
    true
}

fn empty_object() -> Value {
    Value::Object(Map::new())
}

/// A command sent from the bridge to renderers.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(
    tag = "type",
    rename_all = "lowercase",
    rename_all_fields = "camelCase"
)]
pub enum PlayerCommand {
    /// Load and prepare media.
    Load {
        /// Owning session id.
        session_id: String,
        /// Media to load.
        media: PlaybackMediaPayload,
    },
    /// Resume playback.
    Play {
        /// Owning session id.
        session_id: String,
    },
    /// Pause playback.
    Pause {
        /// Owning session id.
        session_id: String,
    },
    /// Seek to a position (seconds).
    Seek {
        /// Owning session id.
        session_id: String,
        /// Target position in seconds.
        position: f64,
    },
    /// Stop playback and tear down.
    Stop {
        /// Owning session id.
        session_id: String,
    },
    /// Update renderer volume.
    Volume {
        /// Owning session id.
        session_id: String,
        /// Level in `[0.0, 1.0]`.
        level: f64,
        /// Mute state.
        muted: bool,
    },
}

impl PlayerCommand {
    /// The session this command targets.
    #[must_use]
    pub fn session_id(&self) -> &str {
        match self {
            PlayerCommand::Load { session_id, .. }
            | PlayerCommand::Play { session_id }
            | PlayerCommand::Pause { session_id }
            | PlayerCommand::Seek { session_id, .. }
            | PlayerCommand::Stop { session_id }
            | PlayerCommand::Volume { session_id, .. } => session_id,
        }
    }
}

/// A report sent from the primary renderer back to the bridge.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "lowercase",
    rename_all_fields = "camelCase"
)]
pub enum PlayerReport {
    /// Player state update.
    State {
        /// Owning session id.
        session_id: String,
        /// Current playback state.
        player_state: PlayerState,
        /// Current position in seconds.
        #[serde(default)]
        current_time: f64,
        /// Total duration in seconds, if known.
        #[serde(default)]
        duration: Option<f64>,
        /// Reason for entering IDLE, if applicable.
        #[serde(default)]
        idle_reason: Option<IdleReason>,
    },
    /// Player error.
    Error {
        /// Owning session id.
        session_id: String,
        /// Error code.
        code: String,
        /// Human-readable message.
        message: String,
    },
}

impl PlayerReport {
    /// The session this report concerns.
    #[must_use]
    pub fn session_id(&self) -> &str {
        match self {
            PlayerReport::State { session_id, .. } | PlayerReport::Error { session_id, .. } => {
                session_id
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn load_command_serializes_camel_case_with_type_tag() {
        let command = PlayerCommand::Load {
            session_id: "s1".into(),
            media: PlaybackMediaPayload {
                streams: vec![PlaybackStreamPayload {
                    url: "https://example.com/manifest.mpd".into(),
                    content_type: "application/dash+xml".into(),
                    drm: None,
                }],
                stream_type: StreamType::Buffered,
                ..Default::default()
            },
        };
        let value = serde_json::to_value(&command).unwrap();
        assert_eq!(value["type"], "load");
        assert_eq!(value["sessionId"], "s1");
        assert_eq!(value["media"]["streamType"], "BUFFERED");
        assert_eq!(
            value["media"]["streams"][0]["url"],
            "https://example.com/manifest.mpd"
        );
        assert_eq!(value["media"]["autoplay"], true);
        assert_eq!(value["media"]["startTime"], 0.0);
        // drm omitted when absent
        assert!(value["media"]["streams"][0].get("drm").is_none());
    }

    #[test]
    fn drm_payload_uses_key_system_identifiers() {
        let value = serde_json::to_value(DrmPayload {
            system: DrmSystem::Widevine,
            license_url: "https://lic".into(),
            headers: HashMap::new(),
        })
        .unwrap();
        assert_eq!(value["system"], "com.widevine.alpha");
        assert_eq!(value["licenseUrl"], "https://lic");
    }

    #[test]
    fn state_report_parses_camel_case() {
        let report: PlayerReport = serde_json::from_value(json!({
            "type": "state",
            "sessionId": "s1",
            "playerState": "PLAYING",
            "currentTime": 21.5
        }))
        .unwrap();
        match report {
            PlayerReport::State {
                session_id,
                player_state,
                current_time,
                ..
            } => {
                assert_eq!(session_id, "s1");
                assert_eq!(player_state, PlayerState::Playing);
                assert_eq!(current_time, 21.5);
            }
            PlayerReport::Error { .. } => panic!("expected state report"),
        }
    }

    #[test]
    fn error_report_parses() {
        let report: PlayerReport = serde_json::from_value(json!({
            "type": "error",
            "sessionId": "s1",
            "code": "LOAD_FAILED",
            "message": "boom"
        }))
        .unwrap();
        assert!(matches!(report, PlayerReport::Error { .. }));
        assert_eq!(report.session_id(), "s1");
    }
}
