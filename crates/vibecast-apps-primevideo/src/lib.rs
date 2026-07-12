//! Bundled Amazon Prime Video app.
//!
//! The launched `PrimeSession` owns all mutable auth/playback state behind a
//! `tokio::Mutex`, per-title license context is scoped to the load that created
//! its route, and Prime's custom Widevine flow is implemented through the SDK
//! license hook.

#![forbid(unsafe_code)]

mod api;
mod models;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Map, Value};
use tokio::sync::Mutex;
use url::Url;
use vibecast_sdk::{
    normalize_stream_type, AppContext, AppManifest, AppProvider, AppSession, DrmInfo, DrmSystem,
    LaunchCredentials, LaunchError, LicenseForwarder, LicenseRequest, LicenseResponse,
    LicenseRoute, LoadRequest, MediaMetadata, MediaResolveCode, MediaResolveError,
    MessageDisposition, PlaybackMedia, PlaybackStream, PlayerCapabilities, StreamSource,
    StreamType,
};

use crate::api::{
    PrimeApiConfig, PrimeError, PrimePlaybackResources, PrimeVideoApi, WidevineLicenseParams,
};
use crate::models::{
    ApplySettingsMessage, NotRegisteredError, PreloadMessage, PrimeMessage, PrimeResponse,
    RegisterMessage,
};

const NS_PRIME: &str = "urn:x-cast:com.amazon.primevideo.cast";
const APP_IDS: &[&str] = &["17608BC8"];
const ICON_URL: &str = "https://lh3.googleusercontent.com/QYGuZRR5YakSPcLFA65pr9BSwrvCpOjcsWiRaMN58t8374iv1HxlRs1mNQm3o0MEq5jmwMtEarN2CLI";
const DEFAULT_MARKETPLACE_ID: &str = "A3K6Y4MI8GDYMT";
const DEFAULT_LOCALE: &str = "en_US";
const DEFAULT_AUTH_BASE_URL: &str = "https://api.amazon.co.uk";

/// Build the Prime API config, translating the player's neutral capabilities
/// into Amazon's request vocabulary.
fn api_config(caps: &PlayerCapabilities) -> PrimeApiConfig {
    PrimeApiConfig {
        auth_base_url: DEFAULT_AUTH_BASE_URL.to_string(),
        display_width: caps.max_resolution.width,
        display_height: caps.max_resolution.height,
        hdcp_level: caps.hdcp_level.clone().unwrap_or_else(|| "1.4".to_string()),
        max_video_resolution: amazon_max_resolution(caps.max_resolution.height),
        supported_codecs: amazon_codecs(&caps.video_codecs),
        dynamic_range_formats: amazon_dynamic_range(&caps.hdr_formats),
        supported_frame_rates: amazon_frame_rates(&caps.frame_rates),
        supported_subtitle_formats: vec!["TTMLv2".to_string(), "DFXP".to_string()],
        ..PrimeApiConfig::default()
    }
}

/// Map a max output height to Amazon's `maxVideoResolution` label.
fn amazon_max_resolution(height: u32) -> String {
    match height {
        0..=576 => "SD",
        577..=720 => "720p",
        721..=1080 => "1080p",
        _ => "UHD",
    }
    .to_string()
}

/// Map neutral video-codec tokens to Amazon's codec vocabulary, preserving order.
fn amazon_codecs(video_codecs: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for codec in video_codecs {
        let mapped = match codec.as_str() {
            "hevc" | "h265" => "H265",
            "h264" | "avc" => "H264",
            _ => continue,
        };
        let mapped = mapped.to_string();
        if !out.contains(&mapped) {
            out.push(mapped);
        }
    }
    if out.is_empty() {
        out.push("H264".to_string());
    }
    out
}

/// Map neutral HDR tokens to Amazon's `dynamicRangeFormats`; SDR reports `None`.
fn amazon_dynamic_range(hdr_formats: &[String]) -> Vec<String> {
    if hdr_formats.is_empty() {
        return vec!["None".to_string()];
    }
    let mut out = Vec::new();
    for format in hdr_formats {
        let mapped = match format.as_str() {
            "hdr10" => "Hdr10",
            "hdr10plus" => "Hdr10Plus",
            "dolbyvision" => "DolbyVision",
            "hlg" => "Hlg",
            _ => continue,
        };
        out.push(mapped.to_string());
    }
    if out.is_empty() {
        out.push("None".to_string());
    }
    out
}

/// Map available frame rates to Amazon's `frameRates` tiers.
fn amazon_frame_rates(frame_rates: &[u32]) -> Vec<String> {
    if frame_rates.iter().any(|fps| *fps > 30) {
        vec!["Standard".to_string(), "High".to_string()]
    } else {
        vec!["Standard".to_string()]
    }
}

/// Prime Video app provider.
#[derive(Debug, Clone, Default)]
pub struct PrimeVideo;

impl PrimeVideo {
    /// Construct the provider.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl AppProvider for PrimeVideo {
    fn manifest(&self) -> AppManifest {
        AppManifest::without_settings("primevideo", APP_IDS, "Prime Video")
            .with_icon_url(ICON_URL)
            .with_namespaces(&[NS_PRIME])
    }

    async fn launch(
        &self,
        ctx: &AppContext,
        credentials: LaunchCredentials,
    ) -> Result<Arc<dyn AppSession>, LaunchError> {
        Ok(Arc::new(PrimeSession {
            api: PrimeVideoApi::new(ctx.http.clone(), api_config(&ctx.receiver.capabilities)),
            state: Mutex::new(PrimeState {
                marketplace_id: DEFAULT_MARKETPLACE_ID.to_string(),
                locale: DEFAULT_LOCALE.to_string(),
                device_id: Some(ctx.receiver.device_id.clone()),
                actor_access_token: credentials.credentials,
                ..PrimeState::default()
            }),
        }))
    }
}

#[derive(Debug, Clone, Default)]
struct TitlePlaybackState {
    playback_envelope: String,
    correlation_id: Option<String>,
    session_handoff_token: Option<String>,
    is_live: bool,
}

#[derive(Debug, Default)]
struct PrimeState {
    marketplace_id: String,
    locale: String,
    actor_id: Option<String>,
    device_id: Option<String>,
    actor_access_token: Option<String>,
    account_refresh_token: Option<String>,
    title_state: HashMap<String, TitlePlaybackState>,
    current_title_id: Option<String>,
}

#[derive(Debug)]
struct ResolveInputs {
    token: String,
    device_id: String,
    marketplace_id: String,
    locale: String,
    title: TitlePlaybackState,
}

/// A running Prime Video session owning its auth and title playback state.
struct PrimeSession {
    api: PrimeVideoApi,
    state: Mutex<PrimeState>,
}

#[async_trait]
impl AppSession for PrimeSession {
    async fn resolve_media(
        &self,
        ctx: &AppContext,
        request: &LoadRequest,
    ) -> Result<PlaybackMedia, MediaResolveError> {
        let title_id = request.media.content_id.trim();
        if title_id.is_empty() {
            return Err(MediaResolveError::invalid_request("INVALID_CONTENT_ID"));
        }

        let stream_type = normalize_stream_type(request.media.stream_type);
        let is_live = stream_type == StreamType::Live;

        let mut inputs = {
            let state = self.state.lock().await;
            let token = state.actor_access_token.clone().ok_or_else(|| {
                MediaResolveError::new(MediaResolveCode::AuthRequired, "NOT_AUTHENTICATED")
            })?;
            let title = preload_for_title(request, &state, title_id);
            if title.playback_envelope.is_empty() {
                return Err(MediaResolveError::new(
                    MediaResolveCode::MissingContext,
                    "NO_PLAYBACK_ENVELOPE",
                ));
            }
            let device_id = device_id(request.custom_data.as_ref(), &state, ctx);
            let marketplace_id = marketplace_id(request.custom_data.as_ref(), &state);
            ResolveInputs {
                token,
                device_id,
                marketplace_id,
                locale: state.locale.clone(),
                title,
            }
        };

        if let Some(correlation_id) = inputs.title.correlation_id.clone() {
            match self
                .api
                .refresh_playback_envelope(
                    &inputs.token,
                    &inputs.device_id,
                    &inputs.marketplace_id,
                    title_id,
                    &correlation_id,
                )
                .await
            {
                Ok(refreshed) => {
                    inputs.title.playback_envelope = refreshed.playback_envelope;
                    inputs.title.correlation_id = refreshed.correlation_id;
                }
                Err(error) => tracing::debug!(%error, title_id, "prime envelope refresh failed"),
            }
        }

        let resources = if is_live {
            self.api
                .get_live_playback_resources(
                    &inputs.token,
                    &inputs.device_id,
                    &inputs.marketplace_id,
                    title_id,
                    &inputs.title.playback_envelope,
                    &inputs.locale,
                )
                .await
        } else {
            self.api
                .get_vod_playback_resources(
                    &inputs.token,
                    &inputs.device_id,
                    &inputs.marketplace_id,
                    title_id,
                    &inputs.title.playback_envelope,
                    &inputs.locale,
                )
                .await
        }
        .map_err(|error| map_prime_error(error, "PRIME_PLAYBACK_RESOURCES"))?;

        let ordered_sets = ordered_url_sets(&resources);
        if ordered_sets.is_empty() {
            return Err(MediaResolveError::content_unavailable("NO_STREAM_URL"));
        }

        let license_url = self.api.widevine_license_url(
            &inputs.device_id,
            &inputs.marketplace_id,
            title_id,
            &inputs.locale,
            is_live,
        );
        let drm = DrmInfo::new(DrmSystem::Widevine, license_url);
        let streams: Vec<PlaybackStream> = ordered_sets
            .into_iter()
            .map(|url| PlaybackStream {
                source: StreamSource::Url(self.api.with_device_type_query(&url)),
                content_type: "application/dash+xml".to_string(),
                drm: Some(drm.clone()),
            })
            .collect();

        inputs.title.session_handoff_token = resources.session_handoff_token;
        inputs.title.is_live = is_live;
        {
            let mut state = self.state.lock().await;
            state.title_state.insert(title_id.to_string(), inputs.title);
            state.current_title_id = Some(title_id.to_string());
            state.device_id = Some(inputs.device_id.clone());
            state.marketplace_id = inputs.marketplace_id.clone();
        }

        let metadata = request.media.metadata.as_ref();
        let mut title = metadata.and_then(metadata_title);
        let mut subtitle = metadata.and_then(metadata_subtitle);
        if title.is_none() || subtitle.is_none() {
            match self
                .api
                .get_catalog_metadata(
                    &inputs.token,
                    &inputs.device_id,
                    &inputs.marketplace_id,
                    title_id,
                    &inputs.locale,
                )
                .await
            {
                Ok(Some(catalog)) => {
                    if title.is_none() {
                        title = catalog.title;
                    }
                    if subtitle.is_none() {
                        subtitle = catalog.subtitle;
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::debug!(%error, title_id, "prime catalog metadata lookup failed")
                }
            }
        }

        Ok(PlaybackMedia {
            session_id: ctx.session_id.clone(),
            streams,
            stream_type,
            content_id: Some(title_id.to_string()),
            title,
            subtitle,
            images: metadata.map(|m| m.images.clone()).unwrap_or_default(),
            duration: positive_duration(request.media.duration),
            autoplay: request.autoplay,
            start_time: request.current_time,
            custom_data: Some(merged_custom_data(&[
                request.media.custom_data.as_ref(),
                request.custom_data.as_ref(),
            ])),
        })
    }

    async fn on_message(
        &self,
        ctx: &AppContext,
        namespace: &str,
        data: &Value,
    ) -> MessageDisposition {
        if namespace != NS_PRIME {
            return MessageDisposition::Unhandled;
        }
        let Ok(message) = serde_json::from_value::<PrimeMessage>(data.clone()) else {
            return MessageDisposition::Unhandled;
        };

        match message {
            PrimeMessage::Register(message) => self.handle_register(ctx, message).await,
            PrimeMessage::AmIRegistered(message) => {
                let response = {
                    let mut state = self.state.lock().await;
                    if let Some(device_id) = non_empty(message.device_id.clone()) {
                        state.device_id = Some(device_id);
                    }
                    if state.actor_access_token.is_some() {
                        PrimeResponse::AmIRegistered { error: None }
                    } else {
                        let device_id = message
                            .device_id
                            .or_else(|| state.device_id.clone())
                            .unwrap_or_else(|| ctx.receiver.device_id.clone());
                        PrimeResponse::AmIRegistered {
                            error: Some(NotRegisteredError {
                                code: "NotRegistered",
                                internal_name: "NotRegistered",
                                message: format!("deviceId {device_id} is not registered"),
                                is_fatal: false,
                            }),
                        }
                    }
                };
                ctx.send_custom(NS_PRIME, response).await;
            }
            PrimeMessage::ApplySettings(message) => {
                self.handle_apply_settings(message).await;
                ctx.send_custom(NS_PRIME, PrimeResponse::ApplySettings)
                    .await;
            }
            PrimeMessage::Preload(message) => {
                self.handle_preload(message).await;
                ctx.send_custom(NS_PRIME, PrimeResponse::Preload).await;
            }
        }

        MessageDisposition::Handled
    }

    async fn resolve_license(
        &self,
        _ctx: &AppContext,
        request: LicenseRequest,
        route: LicenseRoute,
        _forward: &dyn LicenseForwarder,
    ) -> LicenseResponse {
        let (token, device_id, marketplace_id, locale, title_id, title_state) = {
            let state = self.state.lock().await;
            let Some(token) = state.actor_access_token.clone() else {
                return error_license(403, "not authenticated");
            };
            let Some(device_id) = state.device_id.clone() else {
                return error_license(500, "missing device id");
            };
            // Derive the title from the route's own license URL first: it is
            // scoped to the load that created this route, so a license request
            // from an older load never picks up a newer load's title. Fall back
            // to the session's last title only if the route lacks one.
            let title_id =
                title_id_from_url(&route.upstream_url).or_else(|| state.current_title_id.clone());
            let Some(title_id) = title_id else {
                return error_license(400, "missing title id");
            };
            let Some(title_state) = state.title_state.get(&title_id).cloned() else {
                return error_license(409, "missing playback envelope");
            };
            if title_state.playback_envelope.is_empty() {
                return error_license(409, "missing playback envelope");
            }
            (
                token,
                device_id,
                state.marketplace_id.clone(),
                state.locale.clone(),
                title_id,
                title_state,
            )
        };

        match self
            .api
            .get_widevine_license(WidevineLicenseParams {
                token: &token,
                device_id: &device_id,
                marketplace_id: &marketplace_id,
                title_id: &title_id,
                playback_envelope: &title_state.playback_envelope,
                session_handoff_token: title_state.session_handoff_token.as_deref(),
                challenge: &request.body,
                locale: &locale,
                is_live: title_state.is_live,
            })
            .await
        {
            Ok(body) => LicenseResponse::ok(body),
            Err(error) => {
                tracing::warn!(%error, title_id, "prime license request failed");
                error_license(502, "license request failed")
            }
        }
    }
}

impl PrimeSession {
    async fn handle_register(&self, ctx: &AppContext, message: RegisterMessage) {
        let link_flow = {
            let mut state = self.state.lock().await;
            if let Some(marketplace_id) = non_empty(message.marketplace_id.clone()) {
                state.marketplace_id = marketplace_id;
            }
            if let Some(device_id) = non_empty(message.device_id.clone()) {
                state.device_id = Some(device_id);
            }
            if let Some(actor_id) = non_empty(message.actor_id.clone()) {
                state.actor_id = Some(actor_id);
            }

            match (
                non_empty(message.pre_authorized_link_code),
                state.actor_id.clone(),
            ) {
                (Some(link_code), Some(actor_id)) => {
                    let device_id = message
                        .device_id
                        .clone()
                        .or_else(|| state.device_id.clone())
                        .unwrap_or_else(|| ctx.receiver.device_id.clone());
                    Some((link_code, actor_id, device_id))
                }
                _ => None,
            }
        };

        if let Some((link_code, actor_id, device_id)) = link_flow {
            match self.api.register_device(&link_code, &device_id).await {
                Ok(registered) => match self
                    .api
                    .exchange_actor_token(&actor_id, &registered.account_refresh_token)
                    .await
                {
                    Ok(exchanged) => {
                        let mut state = self.state.lock().await;
                        state.account_refresh_token = Some(exchanged.account_refresh_token);
                        state.actor_access_token = Some(exchanged.actor_access_token);
                    }
                    Err(error) => tracing::warn!(%error, "prime token exchange failed"),
                },
                Err(error) => tracing::warn!(%error, "prime register flow failed"),
            }
        }

        ctx.send_custom(NS_PRIME, PrimeResponse::Register).await;
    }

    async fn handle_apply_settings(&self, message: ApplySettingsMessage) {
        let mut state = self.state.lock().await;
        if let Some(locale) = extract_locale(message.settings.as_ref()) {
            state.locale = locale;
        }
        if let Some(device_id) = non_empty(message.device_id) {
            state.device_id = Some(device_id);
        }
    }

    async fn handle_preload(&self, message: PreloadMessage) {
        let (Some(content_id), Some(envelope)) = (message.content_id, message.playback_envelope)
        else {
            return;
        };
        if envelope.envelope.is_empty() {
            return;
        }
        self.state.lock().await.title_state.insert(
            content_id,
            TitlePlaybackState {
                playback_envelope: envelope.envelope,
                correlation_id: envelope.correlation_id,
                ..TitlePlaybackState::default()
            },
        );
    }
}

fn preload_for_title(
    request: &LoadRequest,
    state: &PrimeState,
    title_id: &str,
) -> TitlePlaybackState {
    if let Some(existing) = state.title_state.get(title_id) {
        return existing.clone();
    }
    let Some(Value::Object(custom_data)) = request.custom_data.as_ref() else {
        return TitlePlaybackState::default();
    };
    let Some(Value::Object(envelope)) = custom_data.get("playbackEnvelope") else {
        return TitlePlaybackState::default();
    };
    let Some(Value::String(playback_envelope)) = envelope.get("envelope") else {
        return TitlePlaybackState::default();
    };
    if playback_envelope.is_empty() {
        return TitlePlaybackState::default();
    }
    TitlePlaybackState {
        playback_envelope: playback_envelope.clone(),
        correlation_id: envelope
            .get("correlationId")
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        ..TitlePlaybackState::default()
    }
}

fn device_id(custom_data: Option<&Value>, state: &PrimeState, ctx: &AppContext) -> String {
    get_string(custom_data, "deviceId")
        .or_else(|| state.device_id.clone())
        .unwrap_or_else(|| ctx.receiver.device_id.clone())
}

fn marketplace_id(custom_data: Option<&Value>, state: &PrimeState) -> String {
    get_string(custom_data, "marketplaceId").unwrap_or_else(|| state.marketplace_id.clone())
}

fn get_string(value: Option<&Value>, key: &str) -> Option<String> {
    value
        .and_then(Value::as_object)
        .and_then(|object| object.get(key))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn ordered_url_sets(resources: &PrimePlaybackResources) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut urls: Vec<(String, String)> = resources
        .url_sets
        .iter()
        .filter(|url_set| seen.insert(url_set.url.clone()))
        .map(|url_set| (url_set.url_set_id.clone(), url_set.url.clone()))
        .collect();

    if let Some(default_id) = &resources.default_url_set_id {
        if let Some(index) = urls.iter().position(|(id, _)| id == default_id) {
            let default = urls.remove(index);
            urls.insert(0, default);
        }
    }
    urls.into_iter().map(|(_, url)| url).collect()
}

fn metadata_title(metadata: &MediaMetadata) -> Option<String> {
    non_empty(metadata.title.clone())
}

fn metadata_subtitle(metadata: &MediaMetadata) -> Option<String> {
    non_empty(metadata.subtitle.clone())
}

fn positive_duration(duration: Option<f64>) -> Option<f64> {
    duration.filter(|duration| *duration > 0.0)
}

fn merged_custom_data(items: &[Option<&Value>]) -> Value {
    let mut merged = Map::new();
    for item in items {
        if let Some(Value::Object(object)) = item {
            for (key, value) in object {
                merged.insert(key.clone(), value.clone());
            }
        }
    }
    Value::Object(merged)
}

fn extract_locale(settings: Option<&Value>) -> Option<String> {
    settings
        .and_then(Value::as_object)
        .and_then(|object| object.get("locale"))
        .and_then(Value::as_str)
        .filter(|locale| !locale.is_empty())
        .map(|locale| locale.replace('-', "_"))
}

fn title_id_from_url(url: &str) -> Option<String> {
    Url::parse(url)
        .ok()?
        .query_pairs()
        .find_map(|(key, value)| {
            (key == "titleId" && !value.is_empty()).then(|| value.into_owned())
        })
}

fn non_empty(value: Option<String>) -> Option<String> {
    let trimmed = value?.trim().to_string();
    (!trimmed.is_empty()).then_some(trimmed)
}

fn map_prime_error(error: PrimeError, detail: &str) -> MediaResolveError {
    match error {
        PrimeError::NoPlaybackUrls => MediaResolveError::content_unavailable("NO_STREAM_URL"),
        PrimeError::Http(error) => {
            let mut mapped = MediaResolveError::from(error);
            mapped.detail_code = Some(detail.to_string());
            mapped
        }
        PrimeError::HttpStatus { status, message } => {
            MediaResolveError::from_http_status(status, Some(detail.to_string()))
                .with_message(message)
        }
        other => MediaResolveError::new(MediaResolveCode::UpstreamFailure, detail)
            .with_message(other.to_string()),
    }
}

fn error_license(status: u16, message: &str) -> LicenseResponse {
    LicenseResponse {
        body: message.as_bytes().to_vec(),
        content_type: "application/octet-stream".to_string(),
        status,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex as StdMutex};

    use base64::engine::general_purpose::STANDARD as BASE64;
    use base64::Engine;
    use serde_json::json;
    use vibecast_sdk::{MediaInfo, ReceiverContext, SenderChannel};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    #[derive(Default, Clone)]
    struct RecordingSender {
        messages: Arc<StdMutex<Vec<(String, Value)>>>,
    }

    #[async_trait]
    impl SenderChannel for RecordingSender {
        async fn send_custom(&self, namespace: &str, data: Value) {
            self.messages
                .lock()
                .unwrap()
                .push((namespace.to_string(), data));
        }

        async fn broadcast_custom(&self, namespace: &str, data: Value) {
            self.messages
                .lock()
                .unwrap()
                .push((namespace.to_string(), data));
        }
    }

    fn context(sender: Arc<dyn SenderChannel>, http: reqwest::Client) -> AppContext {
        AppContext::new(
            "sess-1",
            "pid-1",
            "17608BC8",
            http,
            ReceiverContext::new(
                "Living Room",
                "Chromecast",
                "receiver-device-id",
                PathBuf::from("/tmp/vibecast-tests/apps/primevideo"),
            ),
            sender,
        )
    }

    #[test]
    fn api_config_is_derived_from_player_capabilities() {
        use vibecast_sdk::{DrmCapability, DrmSecurityLevel, Platform, Resolution};

        // A 1080p SDR player with hardware Widevine L1 (the SHIELD-on-1080p case).
        let caps = PlayerCapabilities {
            platform: Platform::Android,
            drm: vec![DrmCapability::new(
                DrmSystem::Widevine,
                Some(DrmSecurityLevel::L1),
            )],
            video_codecs: vec!["hevc".to_string(), "h264".to_string()],
            audio_codecs: vec!["aac".to_string(), "eac3".to_string()],
            max_resolution: Resolution::new(1920, 1080),
            hdr_formats: Vec::new(),
            frame_rates: vec![24, 30, 60],
            subtitle_formats: vec!["ttml".to_string()],
            hdcp_level: Some("2.2".to_string()),
        };

        let api = api_config(&caps);
        assert_eq!(api.display_width, 1920);
        assert_eq!(api.display_height, 1080);
        assert_eq!(api.max_video_resolution, "1080p");
        assert_eq!(api.hdcp_level, "2.2");
        assert_eq!(api.supported_codecs, vec!["H265", "H264"]);
        assert_eq!(api.dynamic_range_formats, vec!["None"]); // SDR
        assert_eq!(api.supported_frame_rates, vec!["Standard", "High"]); // 60fps present
    }

    #[test]
    fn capability_mappings_cover_edge_cases() {
        assert_eq!(amazon_max_resolution(480), "SD");
        assert_eq!(amazon_max_resolution(720), "720p");
        assert_eq!(amazon_max_resolution(1080), "1080p");
        assert_eq!(amazon_max_resolution(2160), "UHD");

        // Unknown codecs are dropped; empty falls back to H264.
        assert_eq!(amazon_codecs(&["vp9".to_string()]), vec!["H264"]);
        assert_eq!(
            amazon_codecs(&["h264".to_string(), "hevc".to_string()]),
            vec!["H264", "H265"]
        );

        assert_eq!(amazon_dynamic_range(&[]), vec!["None"]);
        assert_eq!(
            amazon_dynamic_range(&["hdr10".to_string(), "dolbyvision".to_string()]),
            vec!["Hdr10", "DolbyVision"]
        );

        assert_eq!(amazon_frame_rates(&[24, 30]), vec!["Standard"]);
        assert_eq!(amazon_frame_rates(&[24, 50]), vec!["Standard", "High"]);
    }

    fn media(
        content_id: &str,
        stream_type: StreamType,
        metadata: Option<MediaMetadata>,
    ) -> MediaInfo {
        MediaInfo {
            content_id: content_id.to_string(),
            content_type: "video/mp4".to_string(),
            stream_type,
            metadata,
            duration: None,
            custom_data: None,
            content_url: None,
            media_category: None,
            start_absolute_time: None,
            is_live_media: None,
        }
    }

    fn session(server: &MockServer) -> PrimeSession {
        let config = PrimeApiConfig {
            auth_base_url: server.uri(),
            playback_base_url: server.uri(),
            playback_zaz_base_url: server.uri(),
            ..PrimeApiConfig::default()
        };
        PrimeSession {
            api: PrimeVideoApi::new(reqwest::Client::new(), config),
            state: Mutex::new(PrimeState {
                marketplace_id: "A3K6Y4MI8GDYMT".to_string(),
                locale: "en_US".to_string(),
                device_id: Some("receiver-device-id".to_string()),
                actor_access_token: Some("actor-token".to_string()),
                ..PrimeState::default()
            }),
        }
    }

    #[test]
    fn provider_manifest_has_empty_settings() {
        let manifest = PrimeVideo::new().manifest();
        assert_eq!(manifest.app_key, "primevideo");
        assert_eq!(manifest.app_ids, APP_IDS);
        assert_eq!(manifest.display_name, "Prime Video");
        assert_eq!(manifest.icon_url, Some(ICON_URL));
        assert_eq!(manifest.namespaces, &[NS_PRIME]);
        assert!(manifest.settings.settings().is_empty());
    }

    #[tokio::test]
    async fn am_i_registered_returns_not_registered_without_token() {
        let sender = RecordingSender::default();
        let ctx = context(Arc::new(sender.clone()), reqwest::Client::new());
        let session = PrimeSession {
            api: PrimeVideoApi::new(reqwest::Client::new(), PrimeApiConfig::default()),
            state: Mutex::new(PrimeState {
                marketplace_id: "A3K6Y4MI8GDYMT".to_string(),
                locale: "en_US".to_string(),
                device_id: Some("receiver-device-id".to_string()),
                ..PrimeState::default()
            }),
        };

        let disposition = session
            .on_message(
                &ctx,
                NS_PRIME,
                &json!({"type": "AmIRegistered", "deviceId": "cast-device-1"}),
            )
            .await;

        assert_eq!(disposition, MessageDisposition::Handled);
        let messages = sender.messages.lock().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].0, NS_PRIME);
        assert_eq!(messages[0].1["type"], "AmIRegisteredResponse");
        assert_eq!(messages[0].1["error"]["code"], "NotRegistered");
        assert_eq!(
            messages[0].1["error"]["message"],
            "deviceId cast-device-1 is not registered"
        );
    }

    #[tokio::test]
    async fn am_i_registered_returns_success_with_token() {
        let sender = RecordingSender::default();
        let ctx = context(Arc::new(sender.clone()), reqwest::Client::new());
        let session = PrimeSession {
            api: PrimeVideoApi::new(reqwest::Client::new(), PrimeApiConfig::default()),
            state: Mutex::new(PrimeState {
                marketplace_id: "A3K6Y4MI8GDYMT".to_string(),
                locale: "en_US".to_string(),
                actor_access_token: Some("actor-token".to_string()),
                ..PrimeState::default()
            }),
        };

        let _ = session
            .on_message(
                &ctx,
                NS_PRIME,
                &json!({"type": "AmIRegistered", "deviceId": "cast-device-1"}),
            )
            .await;

        let messages = sender.messages.lock().unwrap();
        assert_eq!(messages[0].1, json!({"type": "AmIRegisteredResponse"}));
    }

    #[tokio::test]
    async fn resolve_media_uses_preload_stream_data() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/playback/tags/getRefreshedPlaybackEnvelope"))
            .respond_with(ResponseTemplate::new(500).set_body_string("skip refresh"))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/playback/prs/GetVodPlaybackResources"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "sessionization": {"sessionHandoffToken": "handoff-1"},
                "vodPlaybackUrls": {"result": {"playbackUrls": {
                    "defaultUrlSetId": "dash-main",
                    "urlSets": [
                        {"urlSetId": "dash-main", "url": "https://cdn.example.com/main.mpd"},
                        {"urlSetId": "dash-alt", "url": "https://cdn.example.com/alt.mpd"}
                    ]
                }}}
            })))
            .mount(&server)
            .await;

        let session = session(&server);
        let ctx = context(Arc::new(RecordingSender::default()), reqwest::Client::new());
        session
            .handle_preload(PreloadMessage {
                content_id: Some("amzn1.dv.gti.example".to_string()),
                playback_envelope: Some(crate::models::PreloadEnvelope {
                    envelope: "envelope-v1".to_string(),
                    correlation_id: Some("corr-1".to_string()),
                }),
            })
            .await;

        let request = LoadRequest {
            request_id: 1,
            media: MediaInfo {
                duration: Some(120.0),
                metadata: Some(MediaMetadata {
                    title: Some("Episode 1".to_string()),
                    subtitle: Some("Pilot".to_string()),
                    ..MediaMetadata::default()
                }),
                ..media("amzn1.dv.gti.example", StreamType::Buffered, None)
            },
            autoplay: true,
            current_time: 0.0,
            custom_data: Some(json!({"deviceId": "cast-device-1"})),
        };

        let resolved = session.resolve_media(&ctx, &request).await.unwrap();
        assert_eq!(resolved.streams.len(), 2);
        let stream_url = resolved.streams[0].source.as_url().unwrap();
        assert!(stream_url.starts_with("https://cdn.example.com/main.mpd"));
        assert!(stream_url.contains("amznDtid="));
        let drm = resolved.streams[0].drm.as_ref().unwrap();
        assert_eq!(drm.system, DrmSystem::Widevine);
        assert!(drm
            .license_url
            .contains("/playback/drm-vod/GetWidevineLicense"));
        assert!(drm.license_url.contains("titleId=amzn1.dv.gti.example"));
        assert_eq!(resolved.title.as_deref(), Some("Episode 1"));
        assert_eq!(resolved.subtitle.as_deref(), Some("Pilot"));
        assert_eq!(resolved.duration, Some(120.0));
    }

    #[tokio::test]
    async fn resolve_media_live_uses_live_playback_resources_and_catalog_fallback() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/playback/tags/getRefreshedPlaybackEnvelope"))
            .respond_with(ResponseTemplate::new(500).set_body_string("skip refresh"))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/playback/prs/GetLivePlaybackResources"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "sessionization": {"sessionHandoffToken": "live-handoff-1"},
                "livePlaybackUrls": {"result": {
                    "defaultUrlSetId": "live-main",
                    "urlSets": [
                        {"urlSetId": "live-alt", "urls": {"manifest": {"url": "https://cdn.example.com/live-alt.mpd"}}},
                        {"urlSetId": "live-main", "urls": {"manifest": {"url": "https://cdn.example.com/live-main.mpd"}}}
                    ]
                }}
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/cdp/lumina/playerChromeResources/v1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "resources": {"catalogMetadataV2": {"catalog": {
                    "eventTitle": "Newcastle v Manchester United",
                    "seriesTitle": "Premier League"
                }}}
            })))
            .mount(&server)
            .await;

        let session = session(&server);
        let ctx = context(Arc::new(RecordingSender::default()), reqwest::Client::new());
        let request = LoadRequest {
            request_id: 1,
            media: media(
                "amzn1.dv.gti.live-example",
                StreamType::Live,
                Some(MediaMetadata {
                    title: Some("".to_string()),
                    subtitle: Some("".to_string()),
                    ..MediaMetadata::default()
                }),
            ),
            autoplay: true,
            current_time: 64092211200.0,
            custom_data: Some(json!({
                "deviceId": "cast-device-live",
                "playbackEnvelope": {"envelope": "live-envelope-v1", "correlationId": "live-corr-1"}
            })),
        };

        let resolved = session.resolve_media(&ctx, &request).await.unwrap();
        assert_eq!(resolved.stream_type, StreamType::Live);
        assert_eq!(resolved.start_time, 64092211200.0);
        assert!(resolved.streams[0]
            .source
            .as_url()
            .unwrap()
            .starts_with("https://cdn.example.com/live-main.mpd"));
        assert!(resolved.streams[0]
            .drm
            .as_ref()
            .unwrap()
            .license_url
            .contains("/playback/drm/GetWidevineLicense"));
        assert_eq!(
            resolved.title.as_deref(),
            Some("Newcastle v Manchester United")
        );
        assert_eq!(resolved.subtitle.as_deref(), Some("Premier League"));
    }

    #[tokio::test]
    async fn resolve_license_uses_prime_api() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/playback/drm-vod/GetWidevineLicense"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "widevineLicense": {"license": BASE64.encode(b"license-bytes")}
            })))
            .mount(&server)
            .await;

        let session = session(&server);
        {
            let mut state = session.state.lock().await;
            state.device_id = Some("cast-device-1".to_string());
            state.current_title_id = Some("amzn1.dv.gti.example".to_string());
            state.title_state.insert(
                "amzn1.dv.gti.example".to_string(),
                TitlePlaybackState {
                    playback_envelope: "envelope-v1".to_string(),
                    session_handoff_token: Some("handoff-1".to_string()),
                    ..TitlePlaybackState::default()
                },
            );
        }

        let response = session
            .resolve_license(
                &context(Arc::new(RecordingSender::default()), reqwest::Client::new()),
                LicenseRequest {
                    session_id: "sess-1".to_string(),
                    body: b"abc".to_vec(),
                    content_type: "application/octet-stream".to_string(),
                    route_id: Some("r0".to_string()),
                    headers: vibecast_sdk::HeaderMap::new(),
                },
                LicenseRoute {
                    route_id: "r0".to_string(),
                    system: DrmSystem::Widevine,
                    upstream_url: "https://example.com/license?titleId=amzn1.dv.gti.example"
                        .to_string(),
                    headers: vibecast_sdk::HeaderMap::new(),
                },
                &NoopForwarder,
            )
            .await;

        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"license-bytes");
    }

    #[tokio::test]
    async fn resolve_license_live_uses_live_license_mode() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/playback/drm/GetWidevineLicense"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "widevineLicense": {"license": BASE64.encode(b"live-license-bytes")}
            })))
            .mount(&server)
            .await;

        let session = session(&server);
        {
            let mut state = session.state.lock().await;
            state.device_id = Some("cast-device-1".to_string());
            state.current_title_id = Some("amzn1.dv.gti.live-example".to_string());
            state.title_state.insert(
                "amzn1.dv.gti.live-example".to_string(),
                TitlePlaybackState {
                    playback_envelope: "envelope-live-v1".to_string(),
                    session_handoff_token: Some("handoff-live-1".to_string()),
                    is_live: true,
                    ..TitlePlaybackState::default()
                },
            );
        }

        let response = session
            .resolve_license(
                &context(Arc::new(RecordingSender::default()), reqwest::Client::new()),
                LicenseRequest {
                    session_id: "sess-1".to_string(),
                    body: b"abc".to_vec(),
                    content_type: "application/octet-stream".to_string(),
                    route_id: Some("r0".to_string()),
                    headers: vibecast_sdk::HeaderMap::new(),
                },
                LicenseRoute {
                    route_id: "r0".to_string(),
                    system: DrmSystem::Widevine,
                    upstream_url: "https://example.com/license?titleId=amzn1.dv.gti.live-example"
                        .to_string(),
                    headers: vibecast_sdk::HeaderMap::new(),
                },
                &NoopForwarder,
            )
            .await;

        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"live-license-bytes");
    }

    struct NoopForwarder;

    #[async_trait]
    impl LicenseForwarder for NoopForwarder {
        async fn forward(&self, _request: LicenseRequest, _route: LicenseRoute) -> LicenseResponse {
            LicenseResponse {
                body: Vec::new(),
                content_type: "application/octet-stream".to_string(),
                status: 500,
            }
        }
    }

    #[test]
    fn helper_extracts_title_id_from_license_url() {
        assert_eq!(
            title_id_from_url("https://example.com/license?titleId=amzn1.dv.gti.example"),
            Some("amzn1.dv.gti.example".to_string())
        );
    }

    #[test]
    fn helper_normalizes_locale() {
        assert_eq!(
            extract_locale(Some(&json!({"locale": "sv-SE"}))).as_deref(),
            Some("sv_SE")
        );
    }
}
