//! Serde models for Prime Video custom namespace and API payloads.

use serde::Deserialize;

// -- Cast custom namespace --------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum PrimeMessage {
    #[serde(rename = "Register")]
    Register(RegisterMessage),
    #[serde(rename = "AmIRegistered")]
    AmIRegistered(AmIRegisteredMessage),
    #[serde(rename = "ApplySettings")]
    ApplySettings(ApplySettingsMessage),
    #[serde(rename = "Preload")]
    Preload(PreloadMessage),
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterMessage {
    #[serde(default)]
    pub marketplace_id: Option<String>,
    #[serde(default)]
    pub actor_id: Option<String>,
    #[serde(default)]
    pub pre_authorized_link_code: Option<String>,
    #[serde(default)]
    pub device_id: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AmIRegisteredMessage {
    #[serde(default)]
    pub device_id: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplySettingsMessage {
    #[serde(default)]
    pub device_id: Option<String>,
    #[serde(default)]
    pub settings: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreloadEnvelope {
    pub envelope: String,
    #[serde(default)]
    pub correlation_id: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreloadMessage {
    #[serde(default)]
    pub content_id: Option<String>,
    #[serde(default)]
    pub playback_envelope: Option<PreloadEnvelope>,
}

// -- API responses ----------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AuthRegisterBearerToken {
    #[serde(default)]
    pub refresh_token: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AuthRegisterTokens {
    #[serde(default)]
    pub bearer: Option<AuthRegisterBearerToken>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AuthRegisterSuccess {
    #[serde(default)]
    pub tokens: Option<AuthRegisterTokens>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AuthRegisterResponseContainer {
    #[serde(default)]
    pub success: Option<AuthRegisterSuccess>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AuthRegisterResponse {
    #[serde(default)]
    pub response: Option<AuthRegisterResponseContainer>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct TokenValue {
    #[serde(default)]
    pub token: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AuthTokenDeviceToken {
    #[serde(default)]
    pub actor_access_token: Option<TokenValue>,
    #[serde(default)]
    pub actor_refresh_token: Option<TokenValue>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AuthTokenResponse {
    #[serde(default)]
    pub device_tokens: Vec<AuthTokenDeviceToken>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RefreshedPlaybackExperience {
    #[serde(default)]
    pub correlation_id: Option<String>,
    #[serde(default)]
    pub playback_envelope: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RefreshedEnvelopeItem {
    #[serde(default)]
    pub playback_experience: Option<RefreshedPlaybackExperience>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct RefreshedEnvelopeResponse {
    #[serde(default)]
    pub response: std::collections::HashMap<String, RefreshedEnvelopeItem>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackUrlSetPayload {
    pub url_set_id: String,
    pub url: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackUrlsPayload {
    #[serde(default)]
    pub default_url_set_id: Option<String>,
    #[serde(default)]
    pub url_sets: Vec<PlaybackUrlSetPayload>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VodPlaybackUrlsResult {
    #[serde(default)]
    pub playback_urls: Option<PlaybackUrlsPayload>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct VodPlaybackUrlsSection {
    #[serde(default)]
    pub result: Option<VodPlaybackUrlsResult>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionizationPayload {
    #[serde(default)]
    pub session_handoff_token: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VodPlaybackResourcesResponse {
    #[serde(default)]
    pub sessionization: Option<SessionizationPayload>,
    #[serde(default)]
    pub vod_playback_urls: Option<VodPlaybackUrlsSection>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct LiveManifestPayload {
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct LiveUrlsPayload {
    #[serde(default)]
    pub manifest: Option<LiveManifestPayload>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LivePlaybackUrlSetPayload {
    pub url_set_id: String,
    #[serde(default)]
    pub urls: Option<LiveUrlsPayload>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LivePlaybackUrlsResult {
    #[serde(default)]
    pub default_url_set_id: Option<String>,
    #[serde(default)]
    pub url_sets: Vec<LivePlaybackUrlSetPayload>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct LivePlaybackUrlsSection {
    #[serde(default)]
    pub result: Option<LivePlaybackUrlsResult>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LivePlaybackResourcesResponse {
    #[serde(default)]
    pub sessionization: Option<SessionizationPayload>,
    #[serde(default)]
    pub live_playback_urls: Option<LivePlaybackUrlsSection>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogMetadataPayload {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub event_title: Option<String>,
    #[serde(default)]
    pub series_title: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct CatalogMetadataSection {
    #[serde(default)]
    pub catalog: Option<CatalogMetadataPayload>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerChromeResourcesPayload {
    #[serde(default)]
    pub catalog_metadata_v2: Option<CatalogMetadataSection>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PlayerChromeResourcesResponse {
    #[serde(default)]
    pub resources: Option<PlayerChromeResourcesPayload>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct WidevineLicensePayload {
    #[serde(default)]
    pub license: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WidevineLicenseResponse {
    #[serde(default)]
    pub widevine_license: Option<WidevineLicensePayload>,
}
