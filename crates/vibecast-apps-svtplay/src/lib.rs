//! Bundled SVT Play app.

#![forbid(unsafe_code)]

mod api;
mod models;

use async_trait::async_trait;
use url::Url;
use vibecast_sdk::{
    normalize_stream_type, AppContext, AppProvider, AppSession, LaunchCredentials, LaunchError,
    LoadRequest, MediaMetadata, MediaResolveError, PlaybackMedia, PlaybackStream,
};

use crate::api::{SvtError, SvtPlayApi, SvtResolvedStream};

const APP_IDS: &[&str] = &["95370A1C"];
const ICON_URL: &str = "https://lh3.googleusercontent.com/K3wumlt002dZrHoe4uKKdW-zMRLXdiPdgT1SRP90dnmMvLqsR-zaA3v-360EEIWLL5-SzJVt65XfqlgENw";

/// SVT Play app provider.
#[derive(Debug, Default)]
pub struct SvtPlay;

impl SvtPlay {
    /// Construct the provider.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl AppProvider for SvtPlay {
    fn app_ids(&self) -> &'static [&'static str] {
        APP_IDS
    }

    fn display_name(&self) -> &'static str {
        "SVT Play"
    }

    fn app_key(&self) -> &'static str {
        "svtplay"
    }

    fn icon_url(&self) -> Option<&'static str> {
        Some(ICON_URL)
    }

    async fn launch(
        &self,
        ctx: &AppContext,
        _credentials: LaunchCredentials,
    ) -> Result<Box<dyn AppSession>, LaunchError> {
        Ok(Box::new(SvtSession {
            api: SvtPlayApi::new(ctx.http.clone()),
        }))
    }
}

/// A running SVT Play session that owns its API client.
struct SvtSession {
    api: SvtPlayApi,
}

#[async_trait]
impl AppSession for SvtSession {
    async fn resolve_media(
        &self,
        ctx: &AppContext,
        request: &LoadRequest,
    ) -> Result<PlaybackMedia, MediaResolveError> {
        let media = &request.media;
        let svt_id = extract_svt_id(&media.content_id);
        if svt_id.is_empty() {
            return Err(MediaResolveError::invalid_request("INVALID_CONTENT_ID"));
        }
        tracing::info!(
            session_id = %ctx.session_id,
            svt_id = %svt_id,
            "resolving svt stream"
        );

        let resolved = self
            .api
            .resolve_media(&svt_id, media.custom_data.as_ref())
            .await
            .map_err(map_svt_error)?;

        let streams: Vec<PlaybackStream> = resolved
            .streams
            .into_iter()
            .map(
                |SvtResolvedStream {
                     url,
                     content_type,
                     drm,
                 }| PlaybackStream {
                    url,
                    content_type,
                    drm,
                },
            )
            .collect();
        if streams.is_empty() {
            return Err(MediaResolveError::content_unavailable(
                "NO_RESOLVED_STREAMS",
            ));
        }
        tracing::info!(
            session_id = %ctx.session_id,
            svt_id = %svt_id,
            streams = streams.len(),
            first_url = %streams.first().map(|s| s.url.as_str()).unwrap_or(""),
            "svt stream resolved"
        );

        let metadata = media.metadata.as_ref();
        Ok(PlaybackMedia {
            session_id: ctx.session_id.clone(),
            streams,
            stream_type: normalize_stream_type(media.stream_type),
            content_id: None,
            title: resolved.title.or_else(|| metadata_title(metadata)),
            subtitle: resolved.subtitle.or_else(|| metadata_subtitle(metadata)),
            images: metadata.map(|m| m.images.clone()).unwrap_or_default(),
            duration: resolved.duration.or(media.duration),
            autoplay: request.autoplay,
            start_time: request.current_time,
            custom_data: Some(resolved.custom_data),
        })
    }
}

fn extract_svt_id(content_id: &str) -> String {
    let stripped = content_id.trim();
    if !stripped.contains("://") {
        return stripped.to_string();
    }
    let path = match Url::parse(stripped) {
        Ok(url) => url.path().trim_end_matches('/').to_string(),
        Err(_) => return stripped.to_string(),
    };
    if let Some(index) = path.find("/video/") {
        return path[index + "/video/".len()..].to_string();
    }
    if let Some((_, tail)) = path.rsplit_once('/') {
        if !tail.is_empty() {
            return tail.to_string();
        }
    }
    stripped.to_string()
}

fn metadata_title(metadata: Option<&MediaMetadata>) -> Option<String> {
    metadata.and_then(|m| m.title.clone())
}

fn metadata_subtitle(metadata: Option<&MediaMetadata>) -> Option<String> {
    metadata.and_then(|m| m.subtitle.clone())
}

fn map_svt_error(error: SvtError) -> MediaResolveError {
    match error {
        SvtError::NoDashReference => MediaResolveError::content_unavailable("NO_DASH_REFERENCE"),
        SvtError::NoResolvedStreams => {
            MediaResolveError::content_unavailable("NO_RESOLVED_STREAMS")
        }
        SvtError::Http(error) => {
            let mut mapped = MediaResolveError::from(error);
            mapped.detail_code = Some("SVT_RESOLVE_EXCEPTION".to_string());
            mapped
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use serde_json::json;
    use url::Url;
    use vibecast_sdk::{DrmSystem, MediaInfo, ReceiverContext, StreamType};
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    use crate::api::{SvtApiConfig, SvtPlayApi};

    use super::*;

    fn context() -> AppContext {
        AppContext::new(
            "sess-1",
            "pid-1",
            "95370A1C",
            reqwest::Client::new(),
            ReceiverContext::new(
                "Living Room",
                "Chromecast",
                "receiver-device-id",
                PathBuf::from("/tmp/vibecast-tests/apps/svtplay"),
            ),
            std::sync::Arc::new(vibecast_sdk::NoopSenderChannel),
        )
    }

    fn session(server: &MockServer) -> SvtSession {
        let config = SvtApiConfig {
            video_base: format!("{}/video", server.uri()),
            ditto_endpoint: format!("{}/ditto", server.uri()),
        };
        SvtSession {
            api: SvtPlayApi::with_config(reqwest::Client::new(), config),
        }
    }

    fn load(content_id: &str, media_custom_data: Option<serde_json::Value>) -> LoadRequest {
        LoadRequest {
            request_id: 1,
            media: MediaInfo {
                content_id: content_id.into(),
                content_type: "video/mp4".into(),
                stream_type: StreamType::Buffered,
                metadata: None,
                duration: None,
                custom_data: media_custom_data,
                content_url: None,
                media_category: None,
                start_absolute_time: None,
                is_live_media: None,
            },
            autoplay: true,
            current_time: 12.5,
            custom_data: Some(json!({"topLevelShouldBeIgnored": true})),
        }
    }

    #[test]
    fn extracts_svt_id_from_plain_and_url_forms() {
        assert_eq!(extract_svt_id("egWnL16"), "egWnL16");
        assert_eq!(
            extract_svt_id("https://video.svt.se/video/eXv13pb"),
            "eXv13pb"
        );
        assert_eq!(extract_svt_id("https://www.svtplay.se/foo/bar"), "bar");
    }

    fn video_payload(base: &str) -> serde_json::Value {
        let dash = |name: &str| {
            json!({
                "format": "dash-full",
                "url": format!("{base}/vod/{name}.mpd"),
                "resolve": format!("{base}/resolve/{name}"),
            })
        };
        json!({
            "svtId": "egWnL16",
            "programTitle": "Hundarna",
            "episodeTitle": "1. Nu kor vi!",
            "contentDuration": 2489,
            "videoReferences": [dash("default")],
            "variants": {
                "default": {"videoReferences": [dash("default")]},
                "audioDescribed": {"videoReferences": [dash("audio")]},
                "signInterpreted": {"videoReferences": [dash("sign")]},
            }
        })
    }

    /// Answers ditto / resolve / manifest requests for the plain (no-DRM) case.
    struct Router {
        base: String,
    }

    impl Respond for Router {
        fn respond(&self, request: &Request) -> ResponseTemplate {
            let path = request.url.path();
            let base = &self.base;
            if path.starts_with("/video/egWnL16") {
                return ResponseTemplate::new(200).set_body_json(video_payload(base));
            }
            if path.contains("/resolve/default") {
                return ResponseTemplate::new(200)
                    .set_body_json(json!({"location": format!("{base}/manifest/default.mpd")}));
            }
            if path.contains("/resolve/sign") {
                return ResponseTemplate::new(200)
                    .set_body_json(json!({"location": format!("{base}/manifest/sign.mpd")}));
            }
            if path.contains("/resolve/audio") {
                return ResponseTemplate::new(200)
                    .set_body_json(json!({"location": format!("{base}/manifest/audio.mpd")}));
            }
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/dash+xml")
                .set_body_string("<MPD><Period><AdaptationSet /></Period></MPD>")
        }
    }

    #[tokio::test]
    async fn resolves_manifest_with_alt_variants_and_ditto_params() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(Router { base: server.uri() })
            .mount(&server)
            .await;

        let session = session(&server);
        let media = session
            .resolve_media(
                &context(),
                &load(
                    "egWnL16",
                    Some(json!({
                        "client": "svt-play",
                        "videoTrackPreference": {"kind": "Original"}
                    })),
                ),
            )
            .await
            .expect("resolve should succeed");

        assert!(media.streams.len() >= 2);

        let ditto = Url::parse(&media.streams[0].url).unwrap();
        let params: HashMap<String, String> = ditto.query_pairs().into_owned().collect();
        let manifest = |name: &str| format!("{}/manifest/{name}.mpd", server.uri());
        assert_eq!(params.get("manifestUrl"), Some(&manifest("default")));
        assert_eq!(
            params.get("manifestUrlSignLanguage"),
            Some(&manifest("sign"))
        );
        assert_eq!(
            params.get("manifestUrlAudioDescription"),
            Some(&manifest("audio"))
        );
        assert_eq!(
            params.get("preferredVideoTrack").map(String::as_str),
            Some("original")
        );
        assert_eq!(
            params.get("platform").map(String::as_str),
            Some("chromecast;cc-androidtv")
        );
        assert_eq!(
            params.get("includeAudioCodecs").map(String::as_str),
            Some("mp4a.40.2")
        );
        assert_eq!(params.get("b").map(String::as_str), Some("-6334"));

        assert_eq!(media.streams[1].url, manifest("default"));
        assert_eq!(media.title.as_deref(), Some("Hundarna"));
        assert_eq!(media.subtitle.as_deref(), Some("1. Nu kor vi!"));
        assert_eq!(media.duration, Some(2489.0));
        assert_eq!(media.start_time, 12.5);
        let custom = media.custom_data.unwrap();
        assert_eq!(custom["videoTrackPreference"], json!({"kind": "Original"}));
        assert!(custom.get("topLevelShouldBeIgnored").is_none());
    }

    /// Answers requests for the DRM case: ditto + primary are ClearKey, the
    /// hbbtv fallback is unencrypted.
    struct DrmRouter {
        base: String,
    }

    impl Respond for DrmRouter {
        fn respond(&self, request: &Request) -> ResponseTemplate {
            let path = request.url.path();
            let base = &self.base;
            let clearkey = concat!(
                "<MPD><Period><AdaptationSet><ContentProtection ",
                "schemeIdUri=\"urn:uuid:e2719d58-a985-b3c9-781a-b030af78d30e\">",
                "<dashif:Laurl xmlns:dashif=\"https://dashif.org/guidelines/clearKey\">",
                "https://license.example.com/clearkey</dashif:Laurl>",
                "</ContentProtection></AdaptationSet></Period></MPD>"
            );
            if path.starts_with("/video/cryptAsset") {
                let dash = |name: &str, fmt: &str| json!({"format": fmt, "url": format!("{base}/vod/{name}.mpd"), "resolve": format!("{base}/resolve/{name}")});
                return ResponseTemplate::new(200).set_body_json(json!({
                    "svtId": "cryptAsset",
                    "programTitle": "Encrypted Test",
                    "contentDuration": 111,
                    "videoReferences": [dash("crypt", "dash-full"), dash("hbbtv", "dash-hbbtv-avc")],
                    "variants": {"default": {"videoReferences": [dash("crypt", "dash-full"), dash("hbbtv", "dash-hbbtv-avc")]}}
                }));
            }
            if path.contains("/resolve/crypt") {
                return ResponseTemplate::new(200)
                    .set_body_json(json!({"location": format!("{base}/manifest/crypt.mpd")}));
            }
            if path.contains("/resolve/hbbtv") {
                return ResponseTemplate::new(200)
                    .set_body_json(json!({"location": format!("{base}/manifest/hbbtv.mpd")}));
            }
            if path.contains("/manifest/hbbtv.mpd") {
                return ResponseTemplate::new(200)
                    .insert_header("content-type", "application/dash+xml")
                    .set_body_string("<MPD><Period><AdaptationSet /></Period></MPD>");
            }
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/dash+xml")
                .set_body_string(clearkey)
        }
    }

    #[tokio::test]
    async fn detects_clearkey_and_includes_unencrypted_fallback() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(DrmRouter { base: server.uri() })
            .mount(&server)
            .await;

        let session = session(&server);
        let media = session
            .resolve_media(&context(), &load("cryptAsset", None))
            .await
            .expect("resolve should succeed");

        assert!(
            media.streams.len() >= 3,
            "expected >= 3 streams, got {}",
            media.streams.len()
        );
        assert!(media.streams.iter().any(|stream| stream
            .drm
            .as_ref()
            .is_some_and(|d| d.system == DrmSystem::ClearKey)));
        assert!(media.streams.iter().any(|stream| stream.drm.is_none()));

        let clearkey = media
            .streams
            .iter()
            .find(|stream| stream.drm.is_some())
            .unwrap();
        let drm = clearkey.drm.as_ref().unwrap();
        assert_eq!(drm.system, DrmSystem::ClearKey);
        assert_eq!(drm.license_url, "https://license.example.com/clearkey");
    }

    #[tokio::test]
    async fn invalid_content_id_is_rejected() {
        let server = MockServer::start().await;
        let session = session(&server);
        let result = session.resolve_media(&context(), &load("   ", None)).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().reason(), "INVALID_REQUEST");
    }
}
