//! Typed response models for SVT Play HTTP endpoints.
//!
//! The upstream responses carry many more fields than we use; serde ignores
//! unknown fields by default.

use std::collections::HashMap;

use serde::Deserialize;

/// A single media reference entry from `video.svt.se` responses.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SvtVideoReference {
    /// Direct media URL.
    pub url: String,
    /// Optional resolve endpoint that returns the real location.
    #[serde(default)]
    pub resolve: Option<String>,
    /// Reference format (e.g. `dash-full`, `dash-hbbtv-avc`).
    #[serde(default)]
    pub format: Option<String>,
}

/// Variant-specific references (default / audio-described / sign-language).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SvtVariant {
    /// References for this variant.
    #[serde(default)]
    pub video_references: Vec<SvtVideoReference>,
}

/// Response model for `GET https://video.svt.se/video/{id}`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SvtVideoResponse {
    /// SVT content id.
    pub svt_id: String,
    /// Program title.
    #[serde(default)]
    pub program_title: Option<String>,
    /// Episode title.
    #[serde(default)]
    pub episode_title: Option<String>,
    /// Content duration in seconds.
    #[serde(default)]
    pub content_duration: Option<f64>,
    /// Top-level references.
    #[serde(default)]
    pub video_references: Vec<SvtVideoReference>,
    /// Named variants (values may be null).
    #[serde(default)]
    pub variants: HashMap<String, Option<SvtVariant>>,
}

/// Response model for `switcher.cdn.svt.se/resolve/*` endpoints.
#[derive(Debug, Clone, Deserialize)]
pub struct SvtResolveResponse {
    /// Resolved media location.
    pub location: String,
}
