//! Media namespace messages (`urn:x-cast:com.google.cast.media`).
//!
//! Inbound: GET_STATUS, LOAD, PLAY, PAUSE, SEEK, STOP, SET_VOLUME, QUEUE_*.
//! Outbound: MEDIA_STATUS, LOAD_FAILED, INVALID_REQUEST, QUEUE_ITEM_IDS.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::common::{IdleReason, MediaInfo, PlayerState, Volume, VolumeUpdate};

/// `MediaStatus.supportedMediaCommands` bitmask values. LOAD/PLAY/STOP/
/// GET_STATUS are always implicitly supported and never appear in the mask.
pub mod media_command {
    /// Pause is supported.
    pub const PAUSE: i64 = 1;
    /// Seek is supported.
    pub const SEEK: i64 = 2;
    /// Stream volume change is supported.
    pub const STREAM_VOLUME: i64 = 4;
    /// Stream mute is supported.
    pub const STREAM_MUTE: i64 = 8;

    /// Commands offered while IDLE (no PAUSE — nothing to pause).
    pub const IDLE: i64 = SEEK | STREAM_VOLUME;
    /// Commands offered during active playback.
    pub const ACTIVE: i64 = PAUSE | SEEK | STREAM_VOLUME | STREAM_MUTE;
}

/// Queue repeat mode in MEDIA_STATUS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RepeatMode {
    /// No repeat.
    #[serde(rename = "REPEAT_OFF")]
    RepeatOff,
    /// Repeat the whole queue.
    #[serde(rename = "REPEAT_ALL")]
    RepeatAll,
    /// Repeat the current item.
    #[serde(rename = "REPEAT_SINGLE")]
    RepeatSingle,
    /// Repeat all and shuffle.
    #[serde(rename = "REPEAT_ALL_AND_SHUFFLE")]
    RepeatAllAndShuffle,
}

fn default_true() -> bool {
    true
}

// --- Inbound (sender -> receiver) -----------------------------------------

/// GET_STATUS request.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaGetStatusRequest {
    /// Request id.
    pub request_id: i64,
    /// Optional media session filter.
    #[serde(default)]
    pub media_session_id: Option<i64>,
}

/// LOAD request (app-facing — resolved by the active app's session).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadRequest {
    /// Request id.
    pub request_id: i64,
    /// Media descriptor to load.
    pub media: MediaInfo,
    /// Whether playback starts automatically.
    #[serde(default = "default_true")]
    pub autoplay: bool,
    /// Resume position in seconds.
    #[serde(default)]
    pub current_time: f64,
    /// Top-level custom data (distinct from `media.custom_data`).
    #[serde(default)]
    pub custom_data: Option<Value>,
}

/// PLAY request.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayRequest {
    /// Request id.
    pub request_id: i64,
    /// Media session id.
    pub media_session_id: i64,
}

/// PAUSE request.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PauseRequest {
    /// Request id.
    pub request_id: i64,
    /// Media session id.
    pub media_session_id: i64,
}

/// SEEK request.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeekRequest {
    /// Request id.
    pub request_id: i64,
    /// Media session id.
    pub media_session_id: i64,
    /// Target position in seconds.
    pub current_time: f64,
    /// Optional resume state.
    #[serde(default)]
    pub resume_state: Option<String>,
}

/// Media STOP request (distinct from the receiver STOP).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaStopRequest {
    /// Request id.
    pub request_id: i64,
    /// Media session id.
    pub media_session_id: i64,
}

/// Media SET_VOLUME request (partial volume update).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaSetVolumeRequest {
    /// Request id.
    pub request_id: i64,
    /// Media session id.
    pub media_session_id: i64,
    /// Volume fields to apply.
    pub volume: VolumeUpdate,
}

/// QUEUE_LOAD request (accepted but not queued — single-item playback only).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueLoadRequest {
    /// Request id.
    pub request_id: i64,
}

/// QUEUE_GET_ITEM_IDS request.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueGetItemIdsRequest {
    /// Request id.
    pub request_id: i64,
    /// Optional media session filter.
    #[serde(default)]
    pub media_session_id: Option<i64>,
}

/// Discriminated union of inbound media namespace requests.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MediaRequest {
    /// GET_STATUS.
    GetStatus(MediaGetStatusRequest),
    /// LOAD.
    Load(Box<LoadRequest>),
    /// PLAY.
    Play(PlayRequest),
    /// PAUSE.
    Pause(PauseRequest),
    /// SEEK.
    Seek(SeekRequest),
    /// STOP.
    Stop(MediaStopRequest),
    /// SET_VOLUME.
    SetVolume(MediaSetVolumeRequest),
    /// QUEUE_LOAD.
    QueueLoad(QueueLoadRequest),
    /// QUEUE_GET_ITEM_IDS.
    QueueGetItemIds(QueueGetItemIdsRequest),
}

impl MediaRequest {
    /// The request id carried by any variant.
    #[must_use]
    pub fn request_id(&self) -> i64 {
        match self {
            MediaRequest::GetStatus(r) => r.request_id,
            MediaRequest::Load(r) => r.request_id,
            MediaRequest::Play(r) => r.request_id,
            MediaRequest::Pause(r) => r.request_id,
            MediaRequest::Seek(r) => r.request_id,
            MediaRequest::Stop(r) => r.request_id,
            MediaRequest::SetVolume(r) => r.request_id,
            MediaRequest::QueueLoad(r) => r.request_id,
            MediaRequest::QueueGetItemIds(r) => r.request_id,
        }
    }
}

// --- Sub-models -----------------------------------------------------------

/// Extended status shown during loading (player_state = "LOADING").
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtendedStatus {
    /// Extended player state (e.g. "LOADING").
    pub player_state: String,
    /// Media being prepared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media: Option<MediaInfo>,
    /// Media session id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_session_id: Option<i64>,
}

/// A single media-session status entry within a MEDIA_STATUS response.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaStatus {
    /// Media session id.
    pub media_session_id: i64,
    /// Current media (omitted while IDLE).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media: Option<MediaInfo>,
    /// Playback state.
    #[serde(default)]
    pub player_state: PlayerState,
    /// Current position in seconds.
    #[serde(default)]
    pub current_time: f64,
    /// Supported command bitmask.
    #[serde(default)]
    pub supported_media_commands: i64,
    /// Current volume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume: Option<Volume>,
    /// Idle reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_reason: Option<IdleReason>,
    /// App-specific data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_data: Option<Value>,
    /// Playback rate (1.0 playing, 0.0 otherwise).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub playback_rate: Option<f64>,
    /// Current queue item id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_item_id: Option<i64>,
    /// Queue repeat mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat_mode: Option<RepeatMode>,
    /// Extended (loading) status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extended_status: Option<ExtendedStatus>,
}

// --- Outbound (receiver -> sender) ----------------------------------------

/// MEDIA_STATUS response/broadcast.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaStatusResponse {
    #[serde(rename = "type")]
    kind: &'static str,
    /// Request id (0 for broadcasts).
    pub request_id: i64,
    /// Status entries (empty when there is no active media).
    pub status: Vec<MediaStatus>,
}

impl MediaStatusResponse {
    /// Build a MEDIA_STATUS response.
    #[must_use]
    pub fn new(request_id: i64, status: Vec<MediaStatus>) -> Self {
        Self {
            kind: "MEDIA_STATUS",
            request_id,
            status,
        }
    }
}

/// LOAD_FAILED response.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadFailedResponse {
    #[serde(rename = "type")]
    kind: &'static str,
    /// Request id.
    pub request_id: i64,
    /// Failure reason code.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl LoadFailedResponse {
    /// Build a LOAD_FAILED response.
    #[must_use]
    pub fn new(request_id: i64, reason: impl Into<String>) -> Self {
        Self {
            kind: "LOAD_FAILED",
            request_id,
            reason: Some(reason.into()),
        }
    }
}

/// INVALID_REQUEST response for the media namespace.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaInvalidRequestResponse {
    #[serde(rename = "type")]
    kind: &'static str,
    /// Request id.
    pub request_id: i64,
    /// Failure reason.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl MediaInvalidRequestResponse {
    /// Build an INVALID_REQUEST response.
    #[must_use]
    pub fn new(request_id: i64, reason: impl Into<String>) -> Self {
        Self {
            kind: "INVALID_REQUEST",
            request_id,
            reason: Some(reason.into()),
        }
    }
}

/// QUEUE_ITEM_IDS response.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueItemIdsResponse {
    #[serde(rename = "type")]
    kind: &'static str,
    /// Request id.
    pub request_id: i64,
    /// Item ids.
    pub item_ids: Vec<i64>,
    /// Sequence number.
    pub sequence_number: i64,
}

impl QueueItemIdsResponse {
    /// Build a QUEUE_ITEM_IDS response.
    #[must_use]
    pub fn new(request_id: i64, item_ids: Vec<i64>) -> Self {
        Self {
            kind: "QUEUE_ITEM_IDS",
            request_id,
            item_ids,
            sequence_number: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_load_request_via_union() {
        let request: MediaRequest = serde_json::from_value(json!({
            "type": "LOAD",
            "requestId": 8,
            "media": {"contentId": "egWnL16", "contentType": "video/mp4"},
            "autoplay": true,
            "currentTime": 12.5
        }))
        .unwrap();
        match request {
            MediaRequest::Load(load) => {
                assert_eq!(load.request_id, 8);
                assert_eq!(load.media.content_id, "egWnL16");
                assert_eq!(load.current_time, 12.5);
                assert!(load.autoplay);
            }
            _ => panic!("expected LOAD"),
        }
    }

    #[test]
    fn parses_seek_and_reports_request_id() {
        let request: MediaRequest = serde_json::from_value(json!({
            "type": "SEEK", "requestId": 3, "mediaSessionId": 1, "currentTime": 42.0
        }))
        .unwrap();
        assert_eq!(request.request_id(), 3);
        assert!(matches!(request, MediaRequest::Seek(_)));
    }

    #[test]
    fn unknown_media_type_fails_to_parse() {
        let result: Result<MediaRequest, _> =
            serde_json::from_value(json!({"type": "BOGUS", "requestId": 1}));
        assert!(result.is_err());
    }

    #[test]
    fn media_status_serializes_camel_case_and_omits_none() {
        let status = MediaStatus {
            media_session_id: 2,
            media: None,
            player_state: PlayerState::Playing,
            current_time: 10.0,
            supported_media_commands: media_command::ACTIVE,
            volume: None,
            idle_reason: None,
            custom_data: None,
            playback_rate: Some(1.0),
            current_item_id: Some(1),
            repeat_mode: Some(RepeatMode::RepeatOff),
            extended_status: None,
        };
        let value = serde_json::to_value(MediaStatusResponse::new(5, vec![status])).unwrap();
        assert_eq!(value["type"], "MEDIA_STATUS");
        assert_eq!(value["requestId"], 5);
        assert_eq!(value["status"][0]["playerState"], "PLAYING");
        assert_eq!(value["status"][0]["supportedMediaCommands"], 15);
        assert_eq!(value["status"][0]["repeatMode"], "REPEAT_OFF");
        assert!(value["status"][0].get("media").is_none());
    }

    #[test]
    fn load_failed_serializes() {
        let value = serde_json::to_value(LoadFailedResponse::new(7, "AUTH_REQUIRED")).unwrap();
        assert_eq!(value["type"], "LOAD_FAILED");
        assert_eq!(value["requestId"], 7);
        assert_eq!(value["reason"], "AUTH_REQUIRED");
    }
}
