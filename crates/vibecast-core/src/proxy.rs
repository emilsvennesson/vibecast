//! Session-scoped DRM-license and manifest proxy handler.
//!
//! Handles the proxied license/manifest requests and the stream-URL rewriting.
//! Registered with the player bridge under the session id, it runs inside the
//! bridge's HTTP handler tasks (not the hub actor), so its upstream fetches
//! never block message routing.
//!
//! License requests are dispatched to the app session's `resolve_license`,
//! which by default forwards them via [`DefaultLicenseForwarder`]; apps like
//! Prime Video override it for custom DRM handling.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use http::{HeaderMap, HeaderName, HeaderValue};
use vibecast_player_api::headers::{
    filter_upstream_headers, filter_upstream_response_headers, HOP_BY_HOP_REQUEST_HEADERS,
};
use vibecast_player_api::{
    append_segment_query, default_manifest_content_type, infer_manifest_kind,
    manifest_route_suffix, normalize_manifest_bytes, proxy_hls_children, DrmPayload,
    DrmSystem as WireDrmSystem, LicenseHandler, LicenseRequest as WireLicenseRequest,
    LicenseResponse as WireLicenseResponse, ManifestHandler, ManifestKind, ManifestProxyRequest,
    ManifestProxyResponse, PlaybackMediaPayload, PlaybackStreamPayload, ProxyResult, RouteId,
};
use vibecast_sdk::{
    AppContext, AppSession, DrmSystem, LicenseForwarder, LicenseRequest, LicenseResponse,
    LicenseRoute as SdkLicenseRoute, PlaybackMedia, PlaybackStream, StreamSource,
};

/// A resolved DRM license target.
pub(crate) struct LicenseRoute {
    pub system: DrmSystem,
    pub upstream_url: String,
    pub headers: HeaderMap,
}

/// Where a proxied manifest's bytes come from.
pub(crate) enum ManifestSource {
    /// Fetch (and normalize) the manifest from this upstream URL.
    Upstream(String),
    /// Serve these app-generated manifest bytes verbatim (already absolute).
    Inline(Vec<u8>),
}

/// A resolved manifest target.
pub(crate) struct ManifestRoute {
    pub kind: ManifestKind,
    pub content_type: String,
    pub source: ManifestSource,
    /// Query appended to every segment URL in the manifest (CDN auth tokens).
    pub segment_query: Option<String>,
    /// App-required headers for the upstream fetch; they override the player's.
    pub request_headers: HeaderMap,
    /// Upstream URLs of the HLS child playlists this route has rewritten to
    /// `?child={index}` (only when `segment_query` is set).
    pub children: Mutex<Vec<String>>,
}

impl ManifestRoute {
    /// Apply `segment_query` to a normalized manifest body: HLS master
    /// playlists get their children routed back through `route_file`, media
    /// manifests get the query on every segment URL.
    fn tokenize(&self, body: Vec<u8>, route_file: &str) -> Vec<u8> {
        let Some(query) = &self.segment_query else {
            return body;
        };
        if self.kind == ManifestKind::Hls {
            if let Some(master) =
                proxy_hls_children(&body, route_file, query, |uri| self.child(uri))
            {
                return master;
            }
        }
        append_segment_query(&body, self.kind, query)
    }

    /// The index of child playlist `uri`, registering it on first sight.
    fn child(&self, uri: &str) -> usize {
        let mut children = self.children.lock().unwrap_or_else(PoisonError::into_inner);
        children
            .iter()
            .position(|child| child == uri)
            .unwrap_or_else(|| {
                children.push(uri.to_string());
                children.len() - 1
            })
    }
}

/// Session-scoped proxy handler backing the bridge's license/manifest routes.
pub(crate) struct SessionProxy {
    app: Arc<dyn AppSession>,
    ctx: AppContext,
    license_routes: HashMap<RouteId, LicenseRoute>,
    manifest_routes: HashMap<RouteId, ManifestRoute>,
}

impl SessionProxy {
    pub(crate) fn new(
        app: Arc<dyn AppSession>,
        ctx: AppContext,
        manifest_routes: HashMap<RouteId, ManifestRoute>,
        license_routes: HashMap<RouteId, LicenseRoute>,
    ) -> Self {
        Self {
            app,
            ctx,
            license_routes,
            manifest_routes,
        }
    }
}

#[async_trait]
impl LicenseHandler for SessionProxy {
    async fn handle_license(
        &self,
        request: WireLicenseRequest,
    ) -> ProxyResult<WireLicenseResponse> {
        let Some(route_id) = request.route_id else {
            return Ok(error_license(400, "missing license route"));
        };
        let Some(route) = self.license_routes.get(&route_id) else {
            return Ok(error_license(404, "unknown license route"));
        };

        let req_bytes = request.body.len();
        let upstream_url = route.upstream_url.clone();
        let system = route.system;
        let sdk_request = LicenseRequest {
            session_id: request.session_id.clone(),
            body: request.body,
            content_type: request.content_type,
            route_id: Some(route_id.to_string()),
            headers: request.headers,
        };
        let sdk_route = SdkLicenseRoute {
            route_id: route_id.to_string(),
            system,
            upstream_url: upstream_url.clone(),
            headers: route.headers.clone(),
        };
        let forwarder = DefaultLicenseForwarder {
            http: self.ctx.http.clone(),
        };
        tracing::info!(
            session_id = %request.session_id,
            req_bytes,
            ?system,
            upstream = %upstream_url,
            "license request forwarding"
        );
        let started = tokio::time::Instant::now();
        let response = self
            .app
            .resolve_license(&self.ctx, sdk_request, sdk_route, &forwarder)
            .await;
        match &response {
            LicenseResponse { status, body, .. } if *status >= 200 && *status < 300 => {
                tracing::info!(
                    session_id = %request.session_id,
                    status = *status,
                    resp_bytes = body.len(),
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    "license response"
                );
            }
            LicenseResponse { status, body, .. } => {
                tracing::warn!(
                    session_id = %request.session_id,
                    status = *status,
                    resp_bytes = body.len(),
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    "license response non-2xx"
                );
            }
        }
        Ok(WireLicenseResponse {
            body: response.body,
            content_type: response.content_type,
            status: response.status,
        })
    }
}

#[async_trait]
impl ManifestHandler for SessionProxy {
    async fn handle_manifest(
        &self,
        request: ManifestProxyRequest,
    ) -> ProxyResult<ManifestProxyResponse> {
        let Some(route) = self.manifest_routes.get(&request.route_id) else {
            return Ok(error_manifest(404, "unknown manifest route"));
        };

        let is_head = request.method == http::Method::HEAD;
        let route_file = format!("{}{}", request.route_id, manifest_route_suffix(route.kind));

        let child_url = match request.child {
            Some(index) => {
                let children = route
                    .children
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                match children.get(index) {
                    Some(url) => Some(url.clone()),
                    None => return Ok(error_manifest(404, "unknown child playlist")),
                }
            }
            None => None,
        };

        // An app-generated manifest is served as-is (segment URLs are already
        // absolute, so no normalization or upstream fetch is needed).
        let upstream_url = match (&child_url, &route.source) {
            (Some(url), _) => url,
            (None, ManifestSource::Inline(body)) => {
                let content_type = if route.content_type.is_empty() {
                    default_manifest_content_type(route.kind).to_string()
                } else {
                    route.content_type.clone()
                };
                return Ok(ManifestProxyResponse {
                    body: if is_head {
                        Vec::new()
                    } else {
                        route.tokenize(body.clone(), &route_file)
                    },
                    content_type,
                    status: 200,
                    headers: HeaderMap::new(),
                });
            }
            (None, ManifestSource::Upstream(url)) => url,
        };

        let mut headers = filter_upstream_headers(&request.headers);
        for (name, value) in &route.request_headers {
            headers.insert(name.clone(), value.clone());
        }
        let method = if is_head {
            reqwest::Method::HEAD
        } else {
            reqwest::Method::GET
        };

        let builder = self.ctx.http.request(method, upstream_url).headers(headers);
        let response = match builder.send().await {
            Ok(response) => response,
            Err(error) => {
                tracing::warn!(%error, route = %upstream_url, "manifest fetch failed");
                return Ok(error_manifest(502, "manifest request failed"));
            }
        };

        let status = response.status().as_u16();
        let mut content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
            .unwrap_or_else(|| {
                if route.content_type.is_empty() {
                    default_manifest_content_type(route.kind).to_string()
                } else {
                    route.content_type.clone()
                }
            });
        let response_headers = filter_upstream_response_headers(response.headers());

        if is_head {
            return Ok(ManifestProxyResponse {
                body: Vec::new(),
                content_type,
                status,
                headers: response_headers,
            });
        }

        let mut body = match response.bytes().await {
            Ok(bytes) => bytes.to_vec(),
            Err(error) => {
                tracing::warn!(%error, "manifest body read failed");
                return Ok(error_manifest(502, "manifest request failed"));
            }
        };
        if status < 400 {
            let (normalized, resolved_content_type) =
                normalize_manifest_bytes(&body, upstream_url, Some(&content_type));
            body = route.tokenize(normalized, &route_file);
            content_type = resolved_content_type;
        }

        Ok(ManifestProxyResponse {
            body,
            content_type,
            status,
            headers: response_headers,
        })
    }
}

/// The default license behavior: merge headers and POST to the upstream URL.
struct DefaultLicenseForwarder {
    http: reqwest::Client,
}

impl DefaultLicenseForwarder {
    async fn post(
        &self,
        request: &LicenseRequest,
        route: &SdkLicenseRoute,
    ) -> Result<LicenseResponse, reqwest::Error> {
        // Route headers take precedence; add non-hop-by-hop request headers that
        // the route did not already set.
        let mut headers = route.headers.clone();
        for (name, value) in &request.headers {
            if HOP_BY_HOP_REQUEST_HEADERS.contains(&name.as_str()) {
                continue;
            }
            if !headers.contains_key(name) {
                headers.append(name.clone(), value.clone());
            }
        }
        if let Ok(content_type) = HeaderValue::from_str(&request.content_type) {
            if !request.content_type.is_empty() {
                headers.insert(http::header::CONTENT_TYPE, content_type);
            }
        }

        let response = self
            .http
            .post(&route.upstream_url)
            .headers(headers)
            .body(request.body.clone())
            .send()
            .await?;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("application/octet-stream")
            .to_string();
        let body = response.bytes().await?.to_vec();
        Ok(LicenseResponse {
            body,
            content_type,
            status,
        })
    }
}

#[async_trait]
impl LicenseForwarder for DefaultLicenseForwarder {
    async fn forward(&self, request: LicenseRequest, route: SdkLicenseRoute) -> LicenseResponse {
        match self.post(&request, &route).await {
            Ok(response) => response,
            Err(error) => {
                tracing::warn!(%error, route = %route.upstream_url, "license forward failed");
                LicenseResponse {
                    body: b"app license resolution failed".to_vec(),
                    content_type: "application/octet-stream".to_string(),
                    status: 502,
                }
            }
        }
    }
}

fn error_license(status: u16, message: &str) -> WireLicenseResponse {
    WireLicenseResponse {
        body: message.as_bytes().to_vec(),
        content_type: "application/octet-stream".to_string(),
        status,
    }
}

fn error_manifest(status: u16, message: &str) -> ManifestProxyResponse {
    ManifestProxyResponse {
        body: message.as_bytes().to_vec(),
        content_type: "text/plain".to_string(),
        status,
        headers: HeaderMap::new(),
    }
}

/// Convert an app's string-keyed header config into a validated [`HeaderMap`],
/// silently dropping any entries with invalid header names or values.
fn header_map(map: &HashMap<String, String>) -> HeaderMap {
    let mut headers = HeaderMap::with_capacity(map.len());
    for (name, value) in map {
        if let (Ok(name), Ok(value)) = (
            HeaderName::try_from(name.as_str()),
            HeaderValue::from_str(value),
        ) {
            headers.insert(name, value);
        }
    }
    headers
}

/// The manifest kind to proxy this stream as, or `None` when it is not a
/// manifest (a plain [`StreamSource::Url`] with an unknown kind — e.g. a
/// progressive MP4 — reaches the player directly). Inline manifests are always
/// proxied, classified by their `content_type`.
///
/// `collect_routes` and `rewrite_streams` must agree on this so the registered
/// routes and the rewritten URLs line up.
fn manifest_kind_for(stream: &PlaybackStream) -> Option<ManifestKind> {
    match &stream.source {
        StreamSource::Url(url) => {
            let kind = infer_manifest_kind(Some(&stream.content_type), url);
            (kind != ManifestKind::Unknown).then_some(kind)
        }
        StreamSource::InlineManifest(_) => {
            Some(infer_manifest_kind(Some(&stream.content_type), ""))
        }
    }
}

/// Collect proxy route maps from the resolved media streams.
pub(crate) fn collect_routes(
    media: &PlaybackMedia,
) -> (
    HashMap<RouteId, ManifestRoute>,
    HashMap<RouteId, LicenseRoute>,
) {
    let mut manifest_routes = HashMap::new();
    let mut license_routes = HashMap::new();
    for (index, stream) in media.streams.iter().enumerate() {
        if let Some(kind) = manifest_kind_for(stream) {
            let source = match &stream.source {
                StreamSource::Url(url) => ManifestSource::Upstream(url.clone()),
                StreamSource::InlineManifest(body) => {
                    ManifestSource::Inline(body.clone().into_bytes())
                }
            };
            manifest_routes.insert(
                RouteId::manifest(index),
                ManifestRoute {
                    kind,
                    content_type: stream.content_type.clone(),
                    source,
                    segment_query: stream.segment_query.clone(),
                    request_headers: header_map(&stream.request_headers),
                    children: Mutex::default(),
                },
            );
        }
        if let Some(drm) = &stream.drm {
            license_routes.insert(
                RouteId::license(index),
                LicenseRoute {
                    system: drm.system,
                    upstream_url: drm.license_url.clone(),
                    headers: header_map(&drm.headers),
                },
            );
        }
    }
    (manifest_routes, license_routes)
}

/// Rewrite stream URLs to point at the bridge proxy routes.
///
/// `media_session_id` is appended to each manifest proxy URL as a cache-buster
/// (`?v=...`). Without it, two LOADs against the same session produce identical
/// proxy URLs (`/manifest/{session}/m0.mpd`), and players (browser Shaka,
/// Kodi InputStream) serve the first stream's cached manifest for every
/// subsequent stream switch. Bumping the version per load makes each load a
/// distinct resource so the manifest is always re-fetched.
pub(crate) fn rewrite_streams(
    media: &mut PlaybackMedia,
    manifest_base: Option<&str>,
    license_base: Option<&str>,
    media_session_id: i64,
) {
    for (index, stream) in media.streams.iter_mut().enumerate() {
        if let (Some(kind), Some(base)) = (manifest_kind_for(stream), manifest_base) {
            stream.source = StreamSource::Url(format!(
                "{base}/{route}{}?v={media_session_id}",
                manifest_route_suffix(kind),
                route = RouteId::manifest(index),
            ));
        }
        if let Some(drm) = &mut stream.drm {
            if let Some(base) = license_base {
                drm.license_url = format!("{base}?route={}", RouteId::license(index));
                drm.headers = HashMap::new();
            }
        }
    }
}

/// Convert SDK media into the bridge's player wire payload.
pub(crate) fn to_payload(media: &PlaybackMedia) -> PlaybackMediaPayload {
    PlaybackMediaPayload {
        streams: media
            .streams
            .iter()
            .map(|stream| PlaybackStreamPayload {
                // Proxied streams are rewritten to `Url` before this point; an
                // inline manifest that reached here unrewritten (no proxy base)
                // has no player-facing URL, so it is dropped to an empty string.
                url: match &stream.source {
                    StreamSource::Url(url) => url.clone(),
                    StreamSource::InlineManifest(_) => String::new(),
                },
                content_type: stream.content_type.clone(),
                drm: stream.drm.as_ref().map(|drm| DrmPayload {
                    system: match drm.system {
                        DrmSystem::Widevine => WireDrmSystem::Widevine,
                        DrmSystem::PlayReady => WireDrmSystem::PlayReady,
                        DrmSystem::ClearKey => WireDrmSystem::ClearKey,
                        DrmSystem::FairPlay => WireDrmSystem::FairPlay,
                    },
                    license_url: drm.license_url.clone(),
                    headers: drm.headers.clone(),
                }),
                headers: stream.request_headers.clone(),
            })
            .collect(),
        stream_type: media.stream_type,
        title: media.title.clone(),
        subtitle: media.subtitle.clone(),
        images: media.images.clone(),
        duration: media.duration,
        autoplay: media.autoplay,
        start_time: media.start_time,
        custom_data: media
            .custom_data
            .clone()
            .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use vibecast_player_api::ManifestHandler;
    use vibecast_sdk::{
        LoadRequest, MediaResolveError, NoopSenderChannel, ReceiverContext, StreamType,
    };

    /// A session whose media resolution the manifest proxy never invokes.
    struct UnreachableSession;

    #[async_trait]
    impl AppSession for UnreachableSession {
        async fn resolve_media(
            &self,
            _ctx: &AppContext,
            _request: &LoadRequest,
        ) -> Result<PlaybackMedia, MediaResolveError> {
            unreachable!("the manifest proxy never resolves media")
        }
    }

    fn test_context() -> AppContext {
        AppContext::new(
            "sess",
            "transport",
            "APP",
            reqwest::Client::new(),
            ReceiverContext::new("vibecast", "Model", "dev", PathBuf::new()),
            Arc::new(NoopSenderChannel),
        )
    }

    fn inline_media() -> PlaybackMedia {
        PlaybackMedia::new(
            "sess",
            vec![PlaybackStream::inline_manifest(
                "<MPD>body</MPD>",
                "application/dash+xml",
            )],
            StreamType::Buffered,
        )
    }

    #[test]
    fn inline_stream_registers_inline_manifest_route() {
        let (manifest_routes, license_routes) = collect_routes(&inline_media());
        assert!(license_routes.is_empty());
        let route = manifest_routes
            .get(&RouteId::manifest(0))
            .expect("inline stream should register a manifest route");
        assert!(matches!(route.kind, ManifestKind::Dash));
        assert!(
            matches!(&route.source, ManifestSource::Inline(body) if body == b"<MPD>body</MPD>")
        );
    }

    #[test]
    fn rewrite_and_payload_expose_the_proxy_url_for_inline() {
        let mut media = inline_media();
        rewrite_streams(&mut media, Some("http://proxy/manifest/sess"), None, 7);
        assert_eq!(
            media.streams[0].source.as_url(),
            Some("http://proxy/manifest/sess/m0.mpd?v=7")
        );
        assert_eq!(
            to_payload(&media).streams[0].url,
            "http://proxy/manifest/sess/m0.mpd?v=7"
        );
    }

    #[test]
    fn stream_request_headers_reach_route_and_player_payload() {
        let mut media = PlaybackMedia::new(
            "sess",
            vec![
                PlaybackStream::url("https://cdn.example/stream.mpd", "application/dash+xml")
                    .with_request_header("User-Agent", "Agent/1"),
            ],
            StreamType::Live,
        );
        let (manifest_routes, _) = collect_routes(&media);
        assert_eq!(
            manifest_routes[&RouteId::manifest(0)].request_headers["user-agent"],
            "Agent/1"
        );
        rewrite_streams(&mut media, Some("http://proxy/manifest/sess"), None, 1);
        assert_eq!(
            to_payload(&media).streams[0].headers,
            HashMap::from([("User-Agent".to_string(), "Agent/1".to_string())])
        );
    }

    #[tokio::test]
    async fn upstream_manifest_fetch_uses_app_request_headers_over_the_players() {
        use wiremock::matchers::{header, method};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(header("user-agent", "Agent/1"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "application/dash+xml")
                    .set_body_string("<MPD/>"),
            )
            .expect(1)
            .mount(&server)
            .await;

        let media = PlaybackMedia::new(
            "sess",
            vec![PlaybackStream::url(
                format!("{}/stream.mpd", server.uri()),
                "application/dash+xml",
            )
            .with_request_header("User-Agent", "Agent/1")],
            StreamType::Live,
        );
        let (manifest_routes, _) = collect_routes(&media);
        let proxy = SessionProxy::new(
            Arc::new(UnreachableSession),
            test_context(),
            manifest_routes,
            HashMap::new(),
        );
        let mut player_headers = HeaderMap::new();
        player_headers.insert(
            http::header::USER_AGENT,
            HeaderValue::from_static("Kodi/21"),
        );

        let response = proxy
            .handle_manifest(ManifestProxyRequest {
                session_id: "sess".into(),
                route_id: RouteId::manifest(0),
                method: http::Method::GET,
                headers: player_headers,
                child: None,
            })
            .await
            .unwrap();
        assert_eq!(response.status, 200);
    }

    #[tokio::test]
    async fn hls_segment_query_reaches_child_playlist_segments() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/master.m3u8"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1\nvideo/hi.m3u8\n"),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/video/hi.m3u8"))
            .and(query_param("t", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "#EXTM3U\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"skd://key\"\n\
                 #EXTINF:4.0,\ns1.m4s\n",
            ))
            .expect(1)
            .mount(&server)
            .await;

        let media = PlaybackMedia::new(
            "sess",
            vec![PlaybackStream::url(
                format!("{}/master.m3u8", server.uri()),
                "application/vnd.apple.mpegurl",
            )
            .with_segment_query("t=1")],
            StreamType::Live,
        );
        let (manifest_routes, _) = collect_routes(&media);
        let proxy = SessionProxy::new(
            Arc::new(UnreachableSession),
            test_context(),
            manifest_routes,
            HashMap::new(),
        );
        let request = |child| ManifestProxyRequest {
            session_id: "sess".into(),
            route_id: RouteId::manifest(0),
            method: http::Method::GET,
            headers: HeaderMap::new(),
            child,
        };

        let master = proxy.handle_manifest(request(None)).await.unwrap();
        let master = String::from_utf8(master.body).unwrap();
        assert!(master.contains("\nm0.m3u8?child=0\n"), "{master}");

        let child = proxy.handle_manifest(request(Some(0))).await.unwrap();
        let child = String::from_utf8(child.body).unwrap();
        assert!(
            child.contains(&format!("{}/video/s1.m4s?t=1\n", server.uri())),
            "{child}"
        );
        assert!(child.contains(r#"URI="skd://key""#), "{child}");

        let unknown = proxy.handle_manifest(request(Some(9))).await.unwrap();
        assert_eq!(unknown.status, 404);
    }

    #[tokio::test]
    async fn inline_manifest_segments_get_the_segment_query() {
        let media = PlaybackMedia::new(
            "sess",
            vec![PlaybackStream::inline_manifest(
                "<MPD><SegmentTemplate media=\"https://cdn.example/$Number$.m4s\"/></MPD>",
                "application/dash+xml",
            )
            .with_segment_query("t=1")],
            StreamType::Live,
        );
        let (manifest_routes, _) = collect_routes(&media);
        let proxy = SessionProxy::new(
            Arc::new(UnreachableSession),
            test_context(),
            manifest_routes,
            HashMap::new(),
        );
        let response = proxy
            .handle_manifest(ManifestProxyRequest {
                session_id: "sess".into(),
                route_id: RouteId::manifest(0),
                method: http::Method::GET,
                headers: HeaderMap::new(),
                child: None,
            })
            .await
            .unwrap();
        let body = String::from_utf8(response.body).unwrap();
        assert!(body.contains("$Number$.m4s?t=1"), "{body}");
    }

    #[tokio::test]
    async fn handle_manifest_serves_inline_body_without_upstream_fetch() {
        let ctx = test_context();
        let mut manifest_routes = HashMap::new();
        manifest_routes.insert(
            RouteId::manifest(0),
            ManifestRoute {
                kind: ManifestKind::Dash,
                content_type: "application/dash+xml".to_string(),
                source: ManifestSource::Inline(b"<MPD>inline</MPD>".to_vec()),
                segment_query: None,
                request_headers: HeaderMap::new(),
                children: Mutex::default(),
            },
        );
        let proxy = SessionProxy::new(
            Arc::new(UnreachableSession),
            ctx,
            manifest_routes,
            HashMap::new(),
        );

        let get = proxy
            .handle_manifest(ManifestProxyRequest {
                session_id: "sess".into(),
                route_id: RouteId::manifest(0),
                method: http::Method::GET,
                headers: HeaderMap::new(),
                child: None,
            })
            .await
            .unwrap();
        assert_eq!(get.status, 200);
        assert_eq!(get.content_type, "application/dash+xml");
        assert_eq!(get.body, b"<MPD>inline</MPD>");

        let head = proxy
            .handle_manifest(ManifestProxyRequest {
                session_id: "sess".into(),
                route_id: RouteId::manifest(0),
                method: http::Method::HEAD,
                headers: HeaderMap::new(),
                child: None,
            })
            .await
            .unwrap();
        assert!(head.body.is_empty());
        assert_eq!(head.content_type, "application/dash+xml");
    }
}
