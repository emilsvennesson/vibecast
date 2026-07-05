//! Per-session playback state and MEDIA_STATUS construction.
//!
//! This module owns only the serialized playback state and the status builders.
//! The surrounding IO (sending to senders, driving the renderer, registering
//! proxies) lives in the hub, which owns the transport registry.

use serde_json::Value;
use vibecast_messages::{
    media_command, ExtendedStatus, LoadRequest, MediaCategory, MediaInfo, MediaMetadata,
    MediaStatus, MediaStatusResponse, PlayerState, RepeatMode, StreamType, Volume,
};
use vibecast_sdk::PlaybackMedia;

const LOADING_PLAYER_STATE: &str = "LOADING";

/// Serialized playback state for one app session.
pub(crate) struct Coordinator {
    pub media_session_id: i64,
    pub player_state: PlayerState,
    pub current_time: f64,
    pub idle_reason: Option<vibecast_messages::IdleReason>,
    pub current_media: Option<MediaInfo>,
    pub playback_media: Option<PlaybackMedia>,
    pub volume: Volume,
}

impl Coordinator {
    pub(crate) fn new(volume: Volume) -> Self {
        Self {
            media_session_id: 1,
            player_state: PlayerState::Idle,
            current_time: 0.0,
            idle_reason: None,
            current_media: None,
            playback_media: None,
            volume,
        }
    }

    /// Transition to a terminal IDLE state.
    pub(crate) fn set_idle(&mut self, idle_reason: Option<vibecast_messages::IdleReason>) {
        self.player_state = PlayerState::Idle;
        self.current_time = 0.0;
        self.idle_reason = idle_reason;
    }

    /// Clear the current media descriptors.
    pub(crate) fn clear_media(&mut self) {
        self.current_media = None;
        self.playback_media = None;
    }

    /// Build the current MEDIA_STATUS response (empty status when idle with no
    /// media and no idle reason).
    pub(crate) fn status_response(&self, request_id: i64) -> MediaStatusResponse {
        MediaStatusResponse::new(
            request_id,
            self.media_status()
                .map(|status| vec![status])
                .unwrap_or_default(),
        )
    }

    fn media_status(&self) -> Option<MediaStatus> {
        if self.current_media.is_none() && self.idle_reason.is_none() {
            return None;
        }
        let is_idle = self.player_state == PlayerState::Idle;
        let is_active = matches!(
            self.player_state,
            PlayerState::Playing | PlayerState::Paused | PlayerState::Buffering
        );
        let commands = if is_active {
            media_command::ACTIVE
        } else {
            media_command::IDLE
        };
        let playback_rate = if self.player_state == PlayerState::Playing {
            1.0
        } else {
            0.0
        };
        Some(MediaStatus {
            media_session_id: self.media_session_id,
            media: if is_idle {
                None
            } else {
                self.current_media.clone()
            },
            player_state: self.player_state,
            current_time: self.current_time,
            supported_media_commands: commands,
            volume: Some(self.volume.clone()),
            idle_reason: self.idle_reason,
            custom_data: None,
            playback_rate: Some(playback_rate),
            current_item_id: Some(1),
            repeat_mode: if is_active {
                Some(RepeatMode::RepeatOff)
            } else {
                None
            },
            extended_status: None,
        })
    }

    /// Build an IDLE + LOADING extended status during media resolution.
    pub(crate) fn loading_response(
        &self,
        request_id: i64,
        media: &MediaInfo,
    ) -> MediaStatusResponse {
        let status = MediaStatus {
            media_session_id: self.media_session_id,
            media: Some(media.clone()),
            player_state: PlayerState::Idle,
            current_time: 0.0,
            supported_media_commands: media_command::IDLE,
            volume: Some(self.volume.clone()),
            idle_reason: None,
            custom_data: None,
            playback_rate: Some(1.0),
            current_item_id: Some(1),
            repeat_mode: Some(RepeatMode::RepeatOff),
            extended_status: Some(ExtendedStatus {
                player_state: LOADING_PLAYER_STATE.to_string(),
                media: Some(media.clone()),
                media_session_id: Some(self.media_session_id),
            }),
        };
        MediaStatusResponse::new(request_id, vec![status])
    }
}

/// Build a minimal `MediaInfo` from the original LOAD request for the initial
/// LOADING broadcast (before the app resolves streams).
pub(crate) fn loading_media_info(request: &LoadRequest) -> MediaInfo {
    let content_type = if request.media.content_type.is_empty() {
        "video/*".to_string()
    } else {
        request.media.content_type.clone()
    };
    MediaInfo {
        content_id: request.media.content_id.clone(),
        content_type,
        stream_type: StreamType::None,
        metadata: request.media.metadata.clone(),
        duration: Some(0.0),
        custom_data: None,
        content_url: None,
        media_category: Some(MediaCategory::Video),
        start_absolute_time: None,
        is_live_media: None,
    }
}

/// Build a fully resolved `MediaInfo` from app-resolved media.
pub(crate) fn media_info(media: &PlaybackMedia) -> MediaInfo {
    let primary = media.streams.first();
    let metadata = if media.title.is_some() || media.subtitle.is_some() || !media.images.is_empty()
    {
        Some(MediaMetadata {
            title: media.title.clone(),
            subtitle: media.subtitle.clone(),
            images: media.images.clone(),
            ..MediaMetadata::default()
        })
    } else {
        None
    };
    let content_id = media
        .content_id
        .clone()
        .or_else(|| primary.map(|stream| stream.url.clone()))
        .unwrap_or_default();
    let is_live = media.stream_type == StreamType::Live;
    MediaInfo {
        content_id,
        content_type: primary
            .map(|stream| stream.content_type.clone())
            .unwrap_or_default(),
        stream_type: media.stream_type,
        metadata,
        duration: media.duration,
        custom_data: media.custom_data.clone().filter(is_non_empty_object),
        content_url: primary.map(|stream| stream.url.clone()),
        media_category: Some(MediaCategory::Video),
        start_absolute_time: None,
        is_live_media: if is_live { Some(true) } else { None },
    }
}

fn is_non_empty_object(value: &Value) -> bool {
    !matches!(value, Value::Object(map) if map.is_empty())
}
