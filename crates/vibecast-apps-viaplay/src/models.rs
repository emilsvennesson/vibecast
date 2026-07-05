//! Typed models for the Viaplay Cast namespace and HTTP API.
//!
//! Cast namespace messages use `#[serde(tag = "type")]` for discriminated
//! deserialization.  API response models carry explicit `#[serde(rename)]`
//! for HAL `_links` / `_embedded` keys that contain colons.

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------------------------------------------------------------------------
// Cast namespace — inbound messages (sender → receiver)
// ---------------------------------------------------------------------------

/// Discriminated union of inbound Viaplay Cast namespace messages.
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum ViaplayRequest {
    /// Sender provides receiver setup context after connecting.
    #[serde(rename = "SETUP_INFO", rename_all = "camelCase")]
    SetupInfo {
        #[serde(default)]
        content_root: String,
        #[serde(default)]
        country_code: String,
        #[serde(default)]
        user_id: String,
        #[serde(default)]
        profile_id: String,
        #[serde(default)]
        receiver_name: String,
        #[serde(default = "default_en")]
        receiver_language_code: String,
    },
    /// Sender signals the user activated the device code.
    #[serde(rename = "AUTHORIZATION_DONE", rename_all = "camelCase")]
    AuthorizationDone {
        #[serde(default = "default_true")]
        success: bool,
    },
    /// Sender signals app should return to idle state.
    #[serde(rename = "GOTO_IDLE")]
    GotoIdle {},
}

fn default_en() -> String {
    "en".to_string()
}

fn default_true() -> bool {
    true
}

// ---------------------------------------------------------------------------
// Cast namespace — outbound sub-models (receiver → sender)
// ---------------------------------------------------------------------------

/// Subtitle configuration in receiver state.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubtitleState {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_language_code: Option<String>,
    pub available_language_codes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<Value>,
}

/// Audio track configuration in receiver state.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioTrackState {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_audio_track: Option<String>,
    pub available_audio_tracks: Vec<String>,
}

/// User profile info embedded in receiver state.
#[derive(Debug, Clone, Serialize)]
pub struct UserProfile {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(rename = "type")]
    pub profile_type: String,
}

impl Default for UserProfile {
    fn default() -> Self {
        Self {
            id: None,
            profile_type: "unknown".to_string(),
        }
    }
}

/// Full receiver state broadcast on the Viaplay namespace.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ViaplayReceiverState {
    pub status: String,
    pub is_scrubbable: bool,
    pub pne_in_progress: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_profile: Option<UserProfile>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_display_name: Option<String>,
    pub country_code: String,
    pub receiver_name: String,
    pub receiver_language_code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_product_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loading_product_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authorization_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_code: Option<String>,
    pub subtitles: SubtitleState,
    pub audio_tracks: AudioTrackState,
    pub intro: Value,
    pub recap: Value,
    pub tracking_debug: bool,
    pub feature_flags: Value,
}

/// Outbound Viaplay custom-namespace messages, tagged by `type`.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum ViaplayResponse {
    /// The user must authorize the device via the activation URL / code.
    #[serde(rename = "AUTHORIZATION_REQUIRED", rename_all = "camelCase")]
    AuthorizationRequired {
        /// Activation URL, omitted when unknown.
        #[serde(skip_serializing_if = "Option::is_none")]
        authorization_url: Option<String>,
        /// Full receiver state snapshot.
        receiver_state: ViaplayReceiverState,
    },
    /// A receiver-state update.
    #[serde(rename = "RECEIVER_STATE", rename_all = "camelCase")]
    ReceiverState {
        /// Full receiver state snapshot.
        receiver_state: ViaplayReceiverState,
    },
    /// Authentication succeeded.
    #[serde(rename = "SESSION_OK", rename_all = "camelCase")]
    SessionOk {
        /// Authenticated user id, if known.
        #[serde(skip_serializing_if = "Option::is_none")]
        user_id: Option<String>,
        /// Active profile id, if known.
        #[serde(skip_serializing_if = "Option::is_none")]
        profile_id: Option<String>,
        /// User display name, if known.
        #[serde(skip_serializing_if = "Option::is_none")]
        user_display_name: Option<String>,
        /// Full receiver state snapshot.
        receiver_state: ViaplayReceiverState,
    },
    /// Position / duration progress update.
    #[serde(rename = "POSDUR", rename_all = "camelCase")]
    Posdur {
        /// Current position (whole seconds).
        position: i64,
        /// Total duration (whole seconds).
        duration: i64,
        /// Full receiver state snapshot.
        receiver_state: ViaplayReceiverState,
    },
}

// ---------------------------------------------------------------------------
// Viaplay HTTP API response models
// ---------------------------------------------------------------------------

/// Minimal HAL link with only an `href`.
#[derive(Debug, Clone, Deserialize)]
pub struct HrefLink {
    pub href: String,
}

/// HAL link for an encrypted playlist (DASH or HLS manifest).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct PlaylistLink {
    pub href: String,
    #[serde(default)]
    pub embedded_subtitles: bool,
    #[serde(default)]
    pub streaming_format: String,
}

/// HAL link for a DRM license server.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct LicenseLink {
    pub href: String,
    #[serde(default)]
    pub templated: bool,
    #[serde(default)]
    pub release_pid: String,
}

/// HAL link for a CDN fallback stream.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct FallbackLink {
    pub href: String,
    #[serde(default = "default_dash")]
    pub streaming_format: String,
}

fn default_dash() -> String {
    "Dash".to_string()
}

/// `_links` section of a stream resolution response.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct StreamResponseLinks {
    #[serde(default, rename = "viaplay:encryptedPlaylist")]
    pub encrypted_playlist: Option<PlaylistLink>,
    #[serde(default, rename = "viaplay:playlist")]
    pub playlist: Option<HrefLink>,
    #[serde(default, rename = "viaplay:stream")]
    pub stream: Option<HrefLink>,
    #[serde(default, rename = "viaplay:license")]
    pub license_link: Option<LicenseLink>,
    #[serde(default, rename = "viaplay:widevineLicense")]
    pub widevine_license: Option<LicenseLink>,
    #[serde(default, rename = "viaplay:fallbackMedia")]
    pub fallback_media: Vec<FallbackLink>,
}

/// `product.content` in a stream resolution response.
#[derive(Debug, Clone, Default, Deserialize)]
#[allow(dead_code)]
pub struct StreamProductContent {
    #[serde(default)]
    pub title: String,
    #[serde(default, rename = "type")]
    pub content_type: String,
}

/// `product` in a stream resolution response.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct StreamProduct {
    #[serde(default)]
    pub content: StreamProductContent,
    #[serde(default)]
    pub stream_type: String,
    #[serde(default)]
    pub product_type: String,
}

/// Media object inside `_embedded.viaplay:media`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddedMedia {
    #[serde(default)]
    pub content_url: Option<String>,
    #[serde(default)]
    pub content_type: Option<String>,
}

/// `_embedded` section of a stream resolution response.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct StreamResponseEmbedded {
    #[serde(default, rename = "viaplay:media")]
    pub media: Option<EmbeddedMedia>,
}

/// Top-level response from the Viaplay stream resolution API.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ViaplayStreamResponse {
    #[serde(default)]
    pub duration: f64,
    #[serde(default)]
    pub product: Option<StreamProduct>,
    #[serde(default)]
    pub content_url: Option<String>,
    #[serde(default)]
    pub content_type: Option<String>,
    #[serde(default)]
    pub streaming_format: Option<String>,
    #[serde(default, rename = "_links")]
    pub links: Option<StreamResponseLinks>,
    #[serde(default, rename = "_embedded")]
    pub embedded: Option<StreamResponseEmbedded>,
}

// -- Session check response -------------------------------------------------

/// User data from a content-root session check response.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionUser {
    #[serde(default)]
    pub user_id: String,
    #[serde(default)]
    pub first_name: String,
    #[serde(default)]
    pub last_name: String,
}

/// `_links` section of a session check response.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SessionLinks {
    #[serde(default, rename = "viaplay:persistentLogin")]
    pub persistent_login: Option<HrefLink>,
    #[serde(default, rename = "viaplay:tokenLogin")]
    pub token_login: Option<HrefLink>,
    #[serde(default, rename = "viaplay:deviceAuthorization")]
    pub device_authorization: Option<HrefLink>,
}

/// Response from the content-root session check endpoint.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ViaplaySessionResponse {
    #[serde(default)]
    pub user: Option<SessionUser>,
    #[serde(default, rename = "_links")]
    pub links: Option<SessionLinks>,
}

// -- Device authorization response ------------------------------------------

/// `_links` in a device authorization response.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DeviceAuthLinks {
    #[serde(default, rename = "viaplay:activate")]
    pub activate: Option<HrefLink>,
    #[serde(default, rename = "viaplay:authorized")]
    pub authorized: Option<HrefLink>,
}

/// Response from the device authorization endpoint.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ViaplayDeviceAuthResponse {
    #[serde(default)]
    pub user_code: String,
    #[serde(default)]
    pub device_token: String,
    #[serde(default)]
    pub verification_url: String,
    #[serde(default, rename = "_links")]
    pub links: Option<DeviceAuthLinks>,
}

// -- Authorized poll response -----------------------------------------------

/// `_links` in an authorized poll response.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AuthorizedPollLinks {
    #[serde(default, rename = "viaplay:persistentLogin")]
    pub persistent_login: Option<HrefLink>,
}

/// Response from the authorized poll endpoint.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ViaplayAuthorizedPollResponse {
    #[serde(default, rename = "_links")]
    pub links: Option<AuthorizedPollLinks>,
}
