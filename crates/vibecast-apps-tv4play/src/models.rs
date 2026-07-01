//! Typed response models for the TV4 Play HTTP + GraphQL APIs.
//!
//! `rename_all = "camelCase"` supplies the field aliases the Python pydantic
//! models spelled out by hand; unknown fields are ignored by serde.

use serde::{Deserialize, Serialize};

/// Response from `auth.tv4.a2d.tv/v2/auth/token` (OAuth-style snake_case keys,
/// unlike the camelCase GraphQL / playback APIs).
#[derive(Debug, Clone, Deserialize)]
pub struct Tv4AuthTokenResponse {
    #[serde(default)]
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: String,
}

/// A single image reference.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tv4Image {
    #[serde(default)]
    pub source: Option<String>,
}

/// Known TV4 image variants.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tv4Images {
    #[serde(default)]
    pub main16x9: Option<Tv4Image>,
    #[serde(default)]
    pub poster2x3: Option<Tv4Image>,
    #[serde(default)]
    pub logo: Option<Tv4Image>,
}

/// GraphQL synopsis text.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tv4Synopsis {
    #[serde(default)]
    pub medium: Option<String>,
}

/// Series metadata embedded on episode responses.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tv4Series {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub images: Option<Tv4Images>,
}

/// GraphQL media union (the fields the receiver uses).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tv4Media {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub extended_title: Option<String>,
    #[serde(default)]
    pub images: Option<Tv4Images>,
    #[serde(default)]
    pub synopsis: Option<Tv4Synopsis>,
    #[serde(default)]
    pub series: Option<Tv4Series>,
}

/// Top-level GraphQL response envelope.
#[derive(Debug, Clone, Deserialize)]
pub struct Tv4GraphqlResponse {
    #[serde(default)]
    pub data: Option<Tv4GraphqlData>,
}

/// GraphQL `data` object.
#[derive(Debug, Clone, Deserialize)]
pub struct Tv4GraphqlData {
    #[serde(default)]
    pub media: Option<Tv4Media>,
}

/// Playback metadata returned by `playback2`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tv4PlaybackMetadata {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default, rename = "type")]
    pub media_type: Option<String>,
    #[serde(default)]
    pub duration: Option<f64>,
    #[serde(default)]
    pub is_live: bool,
    #[serde(default)]
    pub image: Option<String>,
}

/// Widevine license info returned by `playback2`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tv4PlaybackLicense {
    #[serde(default)]
    pub castlabs_server: Option<String>,
    #[serde(default)]
    pub castlabs_token: Option<String>,
}

/// Subtitle / text-track metadata (echoed to senders in custom data).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tv4Subtitle {
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Thumbnail sprite metadata (echoed to senders in custom data).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tv4Thumbnail {
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<i64>,
}

/// Resolved playback item from `playback2`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tv4PlaybackItem {
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub manifest_url: Option<String>,
    #[serde(default)]
    pub access_url: Option<String>,
    #[serde(default)]
    pub access_url_type: Option<String>,
    #[serde(default)]
    pub license: Option<Tv4PlaybackLicense>,
    #[serde(default)]
    pub subtitles: Vec<Tv4Subtitle>,
    #[serde(default)]
    pub subs: Vec<Tv4Subtitle>,
    #[serde(default)]
    pub thumbnails: Vec<Tv4Thumbnail>,
}

/// Playback capabilities returned by `playback2`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tv4PlaybackCapabilities {
    #[serde(default = "default_true")]
    pub pause: bool,
    #[serde(default = "default_true")]
    pub seek: bool,
    #[serde(default)]
    pub stream_switch: bool,
}

impl Default for Tv4PlaybackCapabilities {
    fn default() -> Self {
        Self {
            pause: true,
            seek: true,
            stream_switch: false,
        }
    }
}

fn default_true() -> bool {
    true
}

/// Top-level playback response from `playback2`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tv4PlaybackResponse {
    #[serde(default)]
    pub metadata: Option<Tv4PlaybackMetadata>,
    #[serde(default)]
    pub playback_item: Option<Tv4PlaybackItem>,
    #[serde(default)]
    pub capabilities: Tv4PlaybackCapabilities,
}
