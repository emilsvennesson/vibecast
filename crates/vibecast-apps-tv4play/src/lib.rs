//! Bundled TV4 Play app.
//!
//! Redesign of the Python `apps/tv4play`: the owned [`Tv4Session`] holds its
//! mutable auth/playback state behind a `tokio::Mutex` (instead of the Python
//! `_sessions` map), typed serde models replace pydantic, and the legacy
//! custom-namespace snapshot is broadcast through `AppContext::broadcast_custom`.

#![forbid(unsafe_code)]

mod api;
mod models;

use async_trait::async_trait;
use serde_json::{json, Map, Value};
use tokio::sync::Mutex;
use vibecast_sdk::{
    normalize_stream_type, AppContext, AppProvider, AppSession, DrmInfo, DrmSystem,
    LaunchCredentials, LaunchError, LoadRequest, MediaImage, MediaMetadata, MediaResolveCode,
    MediaResolveError, MessageDisposition, PlaybackMedia, PlaybackState, PlaybackStream,
    StreamType,
};

use crate::api::{merged_custom_data, Tv4AuthTokens, Tv4Error, Tv4PlayApi, Tv4ResolvedMedia};
use crate::models::{Tv4Images, Tv4Media, Tv4PlaybackItem, Tv4PlaybackResponse};

const NS_TV4: &str = "urn:x-cast:avod.chromecast";
const APP_IDS: &[&str] = &["B6470434"];
const ICON_URL: &str = "https://cast-receiver.a2d.tv/images/tv4play/logo.svg";

/// TV4 Play app provider.
#[derive(Debug, Default)]
pub struct Tv4Play;

impl Tv4Play {
    /// Construct the provider.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl AppProvider for Tv4Play {
    fn app_ids(&self) -> &'static [&'static str] {
        APP_IDS
    }
    fn display_name(&self) -> &'static str {
        "TV4 Play v5"
    }
    fn app_key(&self) -> &'static str {
        "tv4play"
    }
    fn icon_url(&self) -> Option<&'static str> {
        Some(ICON_URL)
    }
    fn namespaces(&self) -> &'static [&'static str] {
        &[NS_TV4]
    }
    async fn launch(
        &self,
        ctx: &AppContext,
        _credentials: LaunchCredentials,
    ) -> Result<Box<dyn AppSession>, LaunchError> {
        Ok(Box::new(Tv4Session {
            api: Tv4PlayApi::new(ctx.http.clone()),
            state: Mutex::new(Tv4State::default()),
        }))
    }
}

#[derive(Default)]
struct Tv4State {
    tokens: Option<Tv4AuthTokens>,
    asset_id: Option<String>,
    media: Option<PlaybackMedia>,
    playback: Option<Tv4PlaybackResponse>,
    playback_state: Option<PlaybackState>,
}

/// A running TV4 Play session owning its auth + playback state.
struct Tv4Session {
    api: Tv4PlayApi,
    state: Mutex<Tv4State>,
}

#[async_trait]
impl AppSession for Tv4Session {
    async fn resolve_media(
        &self,
        ctx: &AppContext,
        request: &LoadRequest,
    ) -> Result<PlaybackMedia, MediaResolveError> {
        let asset_id = extract_asset_id(request);
        if asset_id.is_empty() {
            return Err(MediaResolveError::invalid_request("INVALID_CONTENT_ID"));
        }
        if asset_id.contains("://") {
            return Ok(direct_media(ctx, request, &asset_id));
        }

        let mut custom_data = merged_custom_data(&[
            request.custom_data.as_ref(),
            request.media.custom_data.as_ref(),
        ]);
        let mut access_token = optional_string(custom_data.get("accessToken"));
        let refresh_token = optional_string(custom_data.get("refreshToken"));
        let profile_id = optional_string(custom_data.get("profileId"));

        if let Some(refresh_token) = refresh_token {
            let tokens = self
                .api
                .refresh_auth(&refresh_token, profile_id.as_deref())
                .await
                .map_err(|error| map_tv4_error(error, "TV4_AUTH_REFRESH_FAILED"))?;
            access_token = Some(tokens.access_token.clone());
            custom_data.insert(
                "refreshToken".to_string(),
                Value::String(tokens.refresh_token.clone()),
            );
            self.state.lock().await.tokens = Some(tokens);
        } else if let Some(tokens) = self.state.lock().await.tokens.as_ref() {
            access_token = Some(tokens.access_token.clone());
        }

        let Some(access_token) = access_token else {
            return Err(MediaResolveError::new(
                MediaResolveCode::AuthRequired,
                "NOT_AUTHENTICATED",
            ));
        };

        let resolved = self
            .api
            .resolve_media(&asset_id, Some(&access_token), &custom_data)
            .await
            .map_err(|error| map_tv4_error(error, "TV4_RESOLVE_FAILED"))?;

        let media = playback_media_from_resolved(ctx, request, &resolved, &custom_data, &asset_id);

        {
            let mut state = self.state.lock().await;
            state.asset_id = Some(asset_id);
            state.media = Some(media.clone());
            state.playback = Some(resolved.playback);
        }
        self.broadcast_snapshot(ctx).await;
        Ok(media)
    }

    async fn on_message(
        &self,
        _ctx: &AppContext,
        namespace: &str,
        _data: &Value,
    ) -> MessageDisposition {
        if namespace == NS_TV4 {
            MessageDisposition::Handled
        } else {
            MessageDisposition::Unhandled
        }
    }

    async fn on_sender_connected(&self, ctx: &AppContext, _sender_id: &str) {
        self.broadcast_snapshot(ctx).await;
    }

    async fn on_playback_update(&self, ctx: &AppContext, state: PlaybackState) {
        let (media, playback_state) = {
            let mut guard = self.state.lock().await;
            guard.playback_state = Some(state);
            (guard.media.clone(), guard.playback_state.clone())
        };
        let Some(media) = media else {
            return;
        };
        broadcast_progress(ctx, &media, playback_state.as_ref()).await;
    }
}

impl Tv4Session {
    async fn broadcast_snapshot(&self, ctx: &AppContext) {
        let snapshot = {
            let state = self.state.lock().await;
            match (state.asset_id.clone(), state.media.clone()) {
                (Some(asset_id), Some(media)) => Some((
                    asset_id,
                    media,
                    state.playback.clone(),
                    state.playback_state.clone(),
                )),
                _ => None,
            }
        };
        let Some((asset_id, media, playback, playback_state)) = snapshot else {
            return;
        };
        ctx.broadcast_custom(NS_TV4, json!({"type": "assetId", "value": asset_id}))
            .await;
        ctx.broadcast_custom(
            NS_TV4,
            json!({"type": "assetMetadata", "value": asset_metadata(&asset_id, &media, playback.as_ref())}),
        )
        .await;
        ctx.broadcast_custom(
            NS_TV4,
            json!({"type": "playbackCapabilities", "value": capabilities(playback.as_ref())}),
        )
        .await;
        broadcast_progress(ctx, &media, playback_state.as_ref()).await;
    }
}

async fn broadcast_progress(
    ctx: &AppContext,
    media: &PlaybackMedia,
    playback_state: Option<&PlaybackState>,
) {
    let duration = playback_state
        .and_then(|s| s.duration)
        .or(media.duration)
        .unwrap_or(0.0)
        .max(0.0);
    let current_time = playback_state.map_or(0.0, |s| s.current_time).max(0.0);
    let message_type = if media.stream_type == StreamType::Live {
        "liveProgressData"
    } else {
        "progressData"
    };
    ctx.broadcast_custom(
        NS_TV4,
        json!({
            "type": message_type,
            "currentTime": current_time,
            "position": current_time,
            "duration": duration,
            "isInAdBreak": false,
            "liveSeekableRange": {"start": 0, "end": duration},
        }),
    )
    .await;
}

fn extract_asset_id(request: &LoadRequest) -> String {
    let content_id = request.media.content_id.trim();
    if !content_id.is_empty() {
        return content_id.to_string();
    }
    if let Some(Value::Object(object)) = request.media.custom_data.as_ref() {
        if let Some(Value::String(asset_id)) = object.get("assetId") {
            let trimmed = asset_id.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }
    String::new()
}

fn optional_string(value: Option<&Value>) -> Option<String> {
    match value {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

fn direct_media(ctx: &AppContext, request: &LoadRequest, content_id: &str) -> PlaybackMedia {
    let media = &request.media;
    let url = media
        .content_url
        .clone()
        .unwrap_or_else(|| content_id.to_string());
    let content_type = if media.content_type.is_empty() {
        content_type_for_url(content_id)
    } else {
        media.content_type.clone()
    };
    let metadata = media.metadata.as_ref();
    let custom_data =
        merged_custom_data(&[request.custom_data.as_ref(), media.custom_data.as_ref()]);
    PlaybackMedia {
        session_id: ctx.session_id.clone(),
        streams: vec![PlaybackStream {
            url,
            content_type,
            drm: None,
        }],
        stream_type: normalize_stream_type(media.stream_type),
        content_id: Some(content_id.to_string()),
        title: metadata.and_then(|m| m.title.clone()),
        subtitle: metadata.and_then(|m| m.subtitle.clone()),
        images: metadata.map(|m| m.images.clone()).unwrap_or_default(),
        duration: media.duration,
        autoplay: request.autoplay,
        start_time: request.current_time,
        custom_data: Some(Value::Object(custom_data)),
    }
}

fn playback_media_from_resolved(
    ctx: &AppContext,
    request: &LoadRequest,
    resolved: &Tv4ResolvedMedia,
    custom_data: &Map<String, Value>,
    asset_id: &str,
) -> PlaybackMedia {
    let playback = &resolved.playback;
    let item = playback.playback_item.as_ref();
    let playback_metadata = playback.metadata.as_ref();
    let metadata = resolved.metadata.as_ref();

    let duration = playback_metadata
        .and_then(|m| m.duration)
        .or(request.media.duration);

    let mut custom_payload = custom_data.clone();
    custom_payload.insert(
        "mediaType".to_string(),
        Value::String(
            playback_metadata
                .and_then(|m| m.media_type.clone())
                .unwrap_or_default(),
        ),
    );
    if let Some(item) = item {
        custom_payload.insert(
            "subtitles".to_string(),
            serde_json::to_value(&item.subtitles).unwrap_or_default(),
        );
        custom_payload.insert(
            "subs".to_string(),
            serde_json::to_value(&item.subs).unwrap_or_default(),
        );
        custom_payload.insert(
            "thumbnails".to_string(),
            serde_json::to_value(&item.thumbnails).unwrap_or_default(),
        );
    }

    PlaybackMedia {
        session_id: ctx.session_id.clone(),
        streams: vec![PlaybackStream {
            url: resolved.manifest_url.clone(),
            content_type: resolved.content_type.clone(),
            drm: drm_info(item),
        }],
        stream_type: stream_type(playback),
        content_id: Some(asset_id.to_string()),
        title: title(metadata, playback_metadata.and_then(|m| m.title.clone())),
        subtitle: subtitle(metadata, request.media.metadata.as_ref()),
        images: images(metadata, playback_metadata.and_then(|m| m.image.as_deref())),
        duration,
        autoplay: request.autoplay,
        start_time: request.current_time,
        custom_data: Some(Value::Object(custom_payload)),
    }
}

fn drm_info(item: Option<&Tv4PlaybackItem>) -> Option<DrmInfo> {
    let license = item?.license.as_ref()?;
    let server = license.castlabs_server.as_ref().filter(|s| !s.is_empty())?;
    let token = license.castlabs_token.as_ref().filter(|t| !t.is_empty())?;
    let mut drm = DrmInfo::new(DrmSystem::Widevine, server.clone());
    drm.headers
        .insert("x-dt-auth-token".to_string(), token.clone());
    Some(drm)
}

fn stream_type(playback: &Tv4PlaybackResponse) -> StreamType {
    if playback.metadata.as_ref().is_some_and(|m| m.is_live) {
        return StreamType::Live;
    }
    if playback
        .playback_item
        .as_ref()
        .and_then(|i| i.state.as_deref())
        == Some("live")
    {
        return StreamType::Live;
    }
    StreamType::Buffered
}

fn title(metadata: Option<&Tv4Media>, fallback: Option<String>) -> Option<String> {
    match metadata {
        Some(media) => media
            .extended_title
            .clone()
            .or_else(|| media.title.clone())
            .or(fallback),
        None => fallback,
    }
}

fn subtitle(metadata: Option<&Tv4Media>, fallback: Option<&MediaMetadata>) -> Option<String> {
    if let Some(media) = metadata {
        if let Some(title) = media.series.as_ref().and_then(|s| s.title.clone()) {
            return Some(title);
        }
        if let Some(medium) = media.synopsis.as_ref().and_then(|s| s.medium.clone()) {
            return Some(medium);
        }
    }
    fallback.and_then(|f| f.subtitle.clone())
}

fn images(metadata: Option<&Tv4Media>, fallback_url: Option<&str>) -> Vec<MediaImage> {
    let mut urls: Vec<String> = Vec::new();
    if let Some(media) = metadata {
        urls.extend(image_urls(media.images.as_ref()));
        if let Some(series) = &media.series {
            urls.extend(image_urls(series.images.as_ref()));
        }
    }
    if let Some(url) = fallback_url {
        urls.push(url.to_string());
    }

    let mut seen = std::collections::HashSet::new();
    urls.into_iter()
        .filter(|url| !url.is_empty() && seen.insert(url.clone()))
        .map(|url| MediaImage {
            url,
            height: None,
            width: None,
        })
        .collect()
}

fn image_urls(images: Option<&Tv4Images>) -> Vec<String> {
    let Some(images) = images else {
        return Vec::new();
    };
    [&images.main16x9, &images.poster2x3, &images.logo]
        .into_iter()
        .filter_map(|image| image.as_ref().and_then(|i| i.source.clone()))
        .collect()
}

fn asset_metadata(
    asset_id: &str,
    media: &PlaybackMedia,
    playback: Option<&Tv4PlaybackResponse>,
) -> Value {
    let media_type = playback
        .and_then(|p| p.metadata.as_ref())
        .and_then(|m| m.media_type.clone())
        .unwrap_or_default();
    json!({
        "id": asset_id,
        "title": media.title,
        "description": media.subtitle,
        "image": media.images.first().map(|image| image.url.clone()),
        "type": media_type,
        "isLive": media.stream_type == StreamType::Live,
    })
}

fn capabilities(playback: Option<&Tv4PlaybackResponse>) -> Value {
    let capabilities = playback.map(|p| &p.capabilities);
    json!({
        "pause": capabilities.is_none_or(|c| c.pause),
        "seek": capabilities.is_none_or(|c| c.seek),
        "skip_ads": false,
        "stream_switch": capabilities.is_some_and(|c| c.stream_switch),
    })
}

fn content_type_for_url(url: &str) -> String {
    let lowered = url.to_lowercase();
    if lowered.contains(".mpd") {
        "application/dash+xml".to_string()
    } else if lowered.contains(".m3u8") {
        "application/x-mpegurl".to_string()
    } else {
        "video/mp4".to_string()
    }
}

fn map_tv4_error(error: Tv4Error, detail: &str) -> MediaResolveError {
    match error {
        Tv4Error::NoManifestUrl => MediaResolveError::content_unavailable("NO_MANIFEST_URL"),
        Tv4Error::NoAccessToken => {
            MediaResolveError::new(MediaResolveCode::UpstreamFailure, detail)
        }
        Tv4Error::Http(error) => {
            let mut mapped = MediaResolveError::from(error);
            mapped.detail_code = Some(detail.to_string());
            mapped
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex as StdMutex};

    use serde_json::json;
    use vibecast_sdk::{MediaInfo, PlayerState, ReceiverContext, SenderChannel};
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    use crate::api::{Tv4ApiConfig, Tv4PlayApi};

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

    fn context(sender: Arc<dyn SenderChannel>) -> AppContext {
        AppContext::new(
            "sess-1",
            "pid-1",
            "B6470434",
            reqwest::Client::new(),
            ReceiverContext::new(
                "Living Room",
                "Chromecast",
                "receiver-device-id",
                PathBuf::from("/tmp/vibecast-tests/apps/tv4play"),
            ),
            sender,
        )
    }

    fn session(server: &MockServer, tokens: Option<Tv4AuthTokens>) -> Tv4Session {
        let config = Tv4ApiConfig {
            auth_url: format!("{}/auth/token", server.uri()),
            graphql_url: format!("{}/graphql", server.uri()),
            playback_base: server.uri(),
        };
        Tv4Session {
            api: Tv4PlayApi::with_config(reqwest::Client::new(), config),
            state: Mutex::new(Tv4State {
                tokens,
                ..Default::default()
            }),
        }
    }

    fn load(content_id: &str, custom_data: Option<Value>) -> LoadRequest {
        LoadRequest {
            request_id: 1,
            media: MediaInfo {
                content_id: content_id.into(),
                content_type: String::new(),
                stream_type: StreamType::None,
                metadata: None,
                duration: None,
                custom_data: None,
                content_url: None,
                media_category: None,
                start_absolute_time: None,
                is_live_media: None,
            },
            autoplay: true,
            current_time: 0.0,
            custom_data,
        }
    }

    fn playback_payload(base: &str, is_live: bool, state: &str, yospace: bool) -> Value {
        let mut item = json!({
            "type": "dash",
            "state": state,
            "manifestUrl": "https://vod.streaming.a2d.tv/original.mpd",
            "license": {
                "castlabsServer": "https://lic.example/wv",
                "castlabsToken": "drm-token-1",
                "type": "widevine",
            },
            "subtitles": [{"type": "vtt", "language": "sv", "url": "https://subs/sv.vtt"}],
            "subs": [],
            "thumbnails": [],
        });
        if yospace {
            item["accessUrl"] = json!(format!("{base}/access"));
            item["accessUrlType"] = json!("yospace");
        }
        json!({
            "id": "asset",
            "metadata": {
                "title": "Playback title",
                "type": if is_live { "channel" } else { "episode" },
                "duration": if is_live { 0 } else { 3019 },
                "isLive": is_live,
                "image": "https://img/fallback.jpg",
            },
            "playbackItem": item,
            "capabilities": {"pause": true, "seek": true, "stream_switch": false},
        })
    }

    #[test]
    fn app_metadata() {
        let app = Tv4Play::new();
        assert_eq!(app.app_ids(), &["B6470434"]);
        assert_eq!(app.display_name(), "TV4 Play v5");
        assert_eq!(app.app_key(), "tv4play");
        assert_eq!(app.namespaces(), &[NS_TV4]);
    }

    struct VodRouter {
        base: String,
    }

    impl Respond for VodRouter {
        fn respond(&self, request: &Request) -> ResponseTemplate {
            let path = request.url.path();
            let base = &self.base;
            if path == "/auth/token" {
                return ResponseTemplate::new(200).set_body_json(json!({
                    "access_token": "access-1",
                    "refresh_token": "refresh-2",
                    "expires_in": 10800000,
                }));
            }
            if path == "/graphql" {
                return ResponseTemplate::new(200).set_body_json(json!({
                    "data": {"media": {
                        "__typename": "Episode",
                        "title": "Avsnitt 1",
                        "extendedTitle": "Coldwater - Avsnitt 1, Sasong 1",
                        "images": {"main16x9": {"source": "https://img/main.jpg"}},
                        "series": {"title": "Coldwater"},
                    }}
                }));
            }
            if path.starts_with("/play/asset-vod") {
                assert_eq!(
                    request.headers.get("x-jwt").and_then(|v| v.to_str().ok()),
                    Some("Bearer access-1")
                );
                return ResponseTemplate::new(200)
                    .set_body_json(playback_payload(base, false, "vod", true));
            }
            if path == "/access" {
                return ResponseTemplate::new(200).set_body_string(concat!(
                    "<Response><MPD href=\"/csm/builder/proxy.1,proxy.2.mpd",
                    "?yo.p.si=abc&amp;ss.sig=sig\" /></Response>"
                ));
            }
            ResponseTemplate::new(404)
        }
    }

    #[tokio::test]
    async fn resolves_vod_with_auth_refresh_and_yospace() {
        let server = MockServer::start().await;
        Mock::given(wiremock::matchers::any())
            .respond_with(VodRouter { base: server.uri() })
            .mount(&server)
            .await;

        let recorder = RecordingSender::default();
        let ctx = context(Arc::new(recorder.clone()));
        let session = session(&server, None);
        let media = session
            .resolve_media(
                &ctx,
                &load(
                    "asset-vod",
                    Some(json!({
                        "refreshToken": "refresh-1",
                        "profileId": "default",
                        "gdpr": "consent-1",
                    })),
                ),
            )
            .await
            .expect("resolve should succeed");

        assert_eq!(media.stream_type, StreamType::Buffered);
        assert_eq!(
            media.streams[0].url,
            format!(
                "{}/csm/builder/proxy.1,proxy.2.mpd?yo.p.si=abc&ss.sig=sig",
                server.uri()
            )
        );
        assert_eq!(media.streams[0].content_type, "application/dash+xml");
        let drm = media.streams[0].drm.as_ref().unwrap();
        assert_eq!(drm.license_url, "https://lic.example/wv");
        assert_eq!(
            drm.headers.get("x-dt-auth-token").map(String::as_str),
            Some("drm-token-1")
        );
        assert_eq!(
            media.title.as_deref(),
            Some("Coldwater - Avsnitt 1, Sasong 1")
        );
        assert_eq!(media.subtitle.as_deref(), Some("Coldwater"));
        assert_eq!(media.images[0].url, "https://img/main.jpg");
        assert_eq!(media.duration, Some(3019.0));
        let custom = media.custom_data.unwrap();
        assert_eq!(custom["refreshToken"], "refresh-2");
        assert_eq!(custom["mediaType"], "episode");

        let messages = recorder.messages.lock().unwrap();
        let types: Vec<&str> = messages
            .iter()
            .map(|(_, v)| v["type"].as_str().unwrap())
            .collect();
        assert_eq!(
            types,
            [
                "assetId",
                "assetMetadata",
                "playbackCapabilities",
                "progressData"
            ]
        );
    }

    struct LiveRouter {
        base: String,
    }

    impl Respond for LiveRouter {
        fn respond(&self, request: &Request) -> ResponseTemplate {
            let path = request.url.path();
            if path == "/graphql" {
                return ResponseTemplate::new(200).set_body_json(json!({
                    "data": {"media": {"__typename": "Channel", "title": "TV4",
                        "images": {"logo": {"source": "https://img/logo.svg"}}}}
                }));
            }
            if path.starts_with("/play/live-asset") {
                return ResponseTemplate::new(200)
                    .set_body_json(playback_payload(&self.base, true, "live", false));
            }
            ResponseTemplate::new(404)
        }
    }

    #[tokio::test]
    async fn resolves_live_with_manifest_url_and_preset_token() {
        let server = MockServer::start().await;
        Mock::given(wiremock::matchers::any())
            .respond_with(LiveRouter { base: server.uri() })
            .mount(&server)
            .await;

        let session = session(
            &server,
            Some(Tv4AuthTokens {
                access_token: "access-1".into(),
                refresh_token: String::new(),
            }),
        );
        let ctx = context(Arc::new(RecordingSender::default()));
        let media = session
            .resolve_media(&ctx, &load("live-asset", None))
            .await
            .expect("resolve should succeed");

        assert_eq!(media.stream_type, StreamType::Live);
        assert_eq!(
            media.streams[0].url,
            "https://vod.streaming.a2d.tv/original.mpd"
        );
        assert_eq!(media.content_id.as_deref(), Some("live-asset"));
        assert_eq!(media.title.as_deref(), Some("TV4"));
        assert_eq!(media.duration, Some(0.0));
    }

    #[tokio::test]
    async fn missing_auth_returns_auth_required() {
        let server = MockServer::start().await;
        let session = session(&server, None);
        let ctx = context(Arc::new(RecordingSender::default()));
        let result = session.resolve_media(&ctx, &load("asset-vod", None)).await;
        let error = result.unwrap_err();
        assert_eq!(error.reason(), "AUTH_REQUIRED");
        assert_eq!(error.detail_code.as_deref(), Some("NOT_AUTHENTICATED"));
    }

    #[tokio::test]
    async fn playback_update_broadcasts_progress() {
        let server = MockServer::start().await;
        let session = session(&server, None);
        {
            let mut media = PlaybackMedia::new("sess-1", vec![], StreamType::Buffered);
            media.duration = Some(120.0);
            let mut state = session.state.lock().await;
            state.asset_id = Some("asset-1".into());
            state.media = Some(media);
        }

        let recorder = RecordingSender::default();
        let ctx = context(Arc::new(recorder.clone()));
        session
            .on_playback_update(
                &ctx,
                PlaybackState {
                    player_state: PlayerState::Playing,
                    current_time: 42.0,
                    duration: Some(120.0),
                    idle_reason: None,
                },
            )
            .await;

        let messages = recorder.messages.lock().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0].1,
            json!({
                "type": "progressData",
                "currentTime": 42.0,
                "position": 42.0,
                "duration": 120.0,
                "isInAdBreak": false,
                "liveSeekableRange": {"start": 0, "end": 120.0},
            })
        );
    }
}
