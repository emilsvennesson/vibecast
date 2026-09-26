//! DAZN custom-namespace messages and Playback API models.
//!
//! The message shapes mirror DAZN's own Chromecast receiver
//! (`tv.dazn.com/app/chromecast`): every message is `{type, data}` on
//! `urn:x-cast:DAZN`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Inbound sender → receiver messages.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum DaznRequest {
    /// Hands the receiver the signed-in user's token and device identity.
    InitSession(InitSession),
    /// Starts playback of an asset.
    InitPlayback(InitPlayback),
    /// Resume playback.
    Play,
    /// Pause playback.
    Pause,
    /// Seek to a position (seconds, relative to the seekable window start).
    Seek(f64),
    /// Stop the current asset but keep the session.
    KillPlayback,
    /// End the session.
    Disconnect,
    /// Audio track preference changed (the player picks tracks itself).
    SetAudioTrack(#[serde(default)] IgnoredData),
    /// Caption track preference changed (the player picks tracks itself).
    SetCaptionTrack(#[serde(default)] IgnoredData),
    /// Diagnostics overlay toggle.
    DiagnosticsVisibilityToggle(#[serde(default)] IgnoredData),
}

/// A message payload the receiver accepts but does not act on.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct IgnoredData(#[serde(default)] serde::de::IgnoredAny);

/// `InitSession` payload.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitSession {
    #[serde(default)]
    pub token: Option<SessionToken>,
    #[serde(default)]
    pub device: Option<SessionDevice>,
    /// Legacy dev-mode identity carrying the device guid.
    #[serde(default)]
    pub marco_polo: Option<SessionDevice>,
    #[serde(default)]
    pub language: Option<String>,
}

impl InitSession {
    /// The DAZN auth token (JWT) supplied by the sender.
    pub fn token(&self) -> Option<&str> {
        self.token
            .as_ref()
            .and_then(|token| token.source.as_deref())
            .filter(|token| !token.is_empty())
    }

    /// The sender's DAZN device guid.
    pub fn device_guid(&self) -> Option<&str> {
        self.device
            .as_ref()
            .or(self.marco_polo.as_ref())
            .and_then(|device| device.guid.as_deref())
            .filter(|guid| !guid.is_empty())
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SessionToken {
    #[serde(default)]
    pub source: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SessionDevice {
    #[serde(default)]
    pub guid: Option<String>,
}

/// `InitPlayback` payload.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitPlayback {
    pub asset_id: String,
    #[serde(default)]
    pub resume_position: Option<ResumePosition>,
    #[serde(default)]
    pub audio_track: Option<TrackPreference>,
    #[serde(default)]
    pub youth_protection_pin: Option<String>,
}

/// Where playback should start: seconds, or a named position.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum ResumePosition {
    Seconds(f64),
    Named(String),
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct TrackPreference {
    #[serde(default)]
    pub language: Option<String>,
}

/// Outbound receiver → sender messages.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", content = "data")]
pub enum DaznMessage {
    /// The session is ready for playback messages.
    SessionInitialized(Option<Value>),
    /// Playback state changed.
    CurrentStateChanged(DaznPlayerState),
    /// Playback position update.
    CurrentTimeChanged(CurrentTime),
    /// A playback or API error occurred (sic: DAZN's own spelling).
    #[serde(rename = "ErrorOccured")]
    ErrorOccurred(DaznErrorPayload),
    /// The receiver is ending the session.
    Disconnect(Option<Value>),
}

/// Video states reported to the sender.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum DaznPlayerState {
    Buffering,
    Ended,
    Error,
    Paused,
    Playing,
    Void,
}

/// `CurrentTimeChanged` payload.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CurrentTime {
    pub current_display_time: i64,
    pub end_display_time: i64,
    pub time_offset: f64,
}

/// `ErrorOccured` payload.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DaznErrorPayload {
    pub app_error_category: u32,
    pub app_error_code: u32,
    pub category: u32,
    pub status: u16,
}

/// `GET /v5/Playback` response.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct PlaybackResponse {
    #[serde(default)]
    pub asset: PlaybackAsset,
    #[serde(default)]
    pub playback_details: Vec<PlaybackDetail>,
    #[serde(default)]
    pub playback_precision: Option<PlaybackPrecision>,
    #[serde(default)]
    pub license: Option<PlaybackLicense>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct PlaybackAsset {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub is_live: bool,
    #[serde(default)]
    pub is_linear: bool,
}

/// One CDN candidate.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct PlaybackDetail {
    #[serde(default)]
    pub manifest_url: Option<String>,
    #[serde(default)]
    pub la_url: Option<String>,
    #[serde(default)]
    pub cdn_name: Option<String>,
    #[serde(default)]
    pub cdn_token: Option<CdnToken>,
    #[serde(default)]
    pub release_pid: Option<String>,
}

/// A CDN token DAZN's player appends as a query parameter to manifest and
/// segment requests.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct CdnToken {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub value: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct PlaybackPrecision {
    #[serde(default)]
    pub cdns: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct PlaybackLicense {
    /// `proxy` (raw challenge POST) or `mpx` (legacy JSON-wrapped).
    #[serde(default)]
    pub mode: Option<String>,
}

/// `RefreshAccessToken` response.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct RefreshResponse {
    #[serde(default)]
    pub auth_token: Option<RefreshedToken>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct RefreshedToken {
    #[serde(default)]
    pub token: Option<String>,
}

/// Legacy MPX Widevine license request body.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MpxLicenseRequest<'a> {
    pub get_widevine_license: MpxLicenseChallenge<'a>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MpxLicenseChallenge<'a> {
    pub release_pid: &'a str,
    pub widevine_challenge: String,
}

/// Legacy MPX Widevine license response body.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MpxLicenseResponse {
    pub get_widevine_license_response: MpxLicense,
}

#[derive(Debug, Deserialize)]
pub struct MpxLicense {
    pub license: String,
}
