//! Session-scoped DRM-license and manifest proxy handler.
//!
//! Ports `PlaybackCoordinator.handle_license` / `handle_manifest` and the
//! stream URL rewriting. Registered with the player bridge under the session
//! id, it runs inside the bridge's HTTP handler tasks (not the hub actor), so
//! its upstream fetches never block message routing.
//!
//! License requests are dispatched to the app session's `resolve_license`,
//! which by default forwards them via [`DefaultLicenseForwarder`]; apps like
//! Prime Video override it for custom DRM handling.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use vibecast_bridge::headers::{
    filter_upstream_headers, filter_upstream_response_headers, HOP_BY_HOP_REQUEST_HEADERS,
};
use vibecast_bridge::{
    default_manifest_content_type, infer_manifest_kind, manifest_route_suffix,
    normalize_manifest_bytes, DrmPayload, DrmSystem as WireDrmSystem, LicenseHandler,
    LicenseRequest as WireLicenseRequest, LicenseResponse as WireLicenseResponse, ManifestHandler,
    ManifestKind, ManifestProxyRequest, ManifestProxyResponse, PlaybackMediaPayload,
    PlaybackStreamPayload, ProxyResult,
};
use vibecast_sdk::{
    AppContext, AppSession, DrmSystem, LicenseForwarder, LicenseRequest, LicenseResponse,
    LicenseRoute as SdkLicenseRoute, PlaybackMedia,
};

/// A resolved DRM license target.
pub(crate) struct LicenseRoute {
    pub system: DrmSystem,
    pub upstream_url: String,
    pub headers: HashMap<String, String>,
}

/// A resolved manifest target.
pub(crate) struct ManifestRoute {
    pub kind: ManifestKind,
    pub upstream_url: String,
    pub content_type: String,
}

/// Session-scoped proxy handler backing the bridge's license/manifest routes.
pub(crate) struct SessionProxy {
    app_key: String,
    app: Arc<dyn AppSession>,
    ctx: AppContext,
    license_routes: HashMap<String, LicenseRoute>,
    manifest_routes: HashMap<String, ManifestRoute>,
}

impl SessionProxy {
    pub(crate) fn new(
        app_key: String,
        app: Arc<dyn AppSession>,
        ctx: AppContext,
        manifest_routes: HashMap<String, ManifestRoute>,
        license_routes: HashMap<String, LicenseRoute>,
    ) -> Self {
        Self {
            app_key,
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
        let Some(route_id) = request.route_id.clone() else {
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
            route_id: Some(route_id.clone()),
            headers: request.headers,
        };
        let sdk_route = SdkLicenseRoute {
            route_id,
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

        let headers = filter_upstream_headers(&request.headers);
        let is_head = request.method.eq_ignore_ascii_case("HEAD");
        let method = if is_head {
            reqwest::Method::HEAD
        } else {
            reqwest::Method::GET
        };

        let mut builder = self.ctx.http.request(method, &route.upstream_url);
        for (key, value) in &headers {
            builder = builder.header(key, value);
        }
        let response = match builder.send().await {
            Ok(response) => response,
            Err(error) => {
                tracing::warn!(%error, route = %route.upstream_url, "manifest fetch failed");
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
        let response_headers =
            filter_upstream_response_headers(&reqwest_headers_to_map(response.headers()));

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
            let (normalized, resolved_content_type) = normalize_manifest_bytes(
                &body,
                &route.upstream_url,
                Some(&content_type),
                Some(&self.app_key),
            );
            body = normalized;
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
        // Route headers take precedence; add non-hop-by-hop request headers.
        let mut headers = route.headers.clone();
        let mut names: HashSet<String> = headers.keys().map(|k| k.to_ascii_lowercase()).collect();
        for (key, value) in &request.headers {
            let lowered = key.to_ascii_lowercase();
            if HOP_BY_HOP_REQUEST_HEADERS.contains(&lowered.as_str()) {
                continue;
            }
            if !names.contains(&lowered) {
                headers.insert(key.clone(), value.clone());
                names.insert(lowered);
            }
        }
        if !request.content_type.is_empty() {
            headers.insert("Content-Type".to_string(), request.content_type.clone());
        }

        let mut builder = self
            .http
            .post(&route.upstream_url)
            .body(request.body.clone());
        for (key, value) in &headers {
            builder = builder.header(key, value);
        }
        let response = builder.send().await?;
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
        headers: HashMap::new(),
    }
}

fn reqwest_headers_to_map(headers: &reqwest::header::HeaderMap) -> HashMap<String, String> {
    headers
        .iter()
        .filter_map(|(key, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (key.as_str().to_string(), value.to_string()))
        })
        .collect()
}

/// Collect proxy route maps from the resolved media streams.
pub(crate) fn collect_routes(
    media: &PlaybackMedia,
) -> (
    HashMap<String, ManifestRoute>,
    HashMap<String, LicenseRoute>,
) {
    let mut manifest_routes = HashMap::new();
    let mut license_routes = HashMap::new();
    for (index, stream) in media.streams.iter().enumerate() {
        let kind = infer_manifest_kind(Some(&stream.content_type), &stream.url);
        if kind != ManifestKind::Unknown {
            manifest_routes.insert(
                format!("m{index}"),
                ManifestRoute {
                    kind,
                    upstream_url: stream.url.clone(),
                    content_type: stream.content_type.clone(),
                },
            );
        }
        if let Some(drm) = &stream.drm {
            license_routes.insert(
                format!("r{index}"),
                LicenseRoute {
                    system: drm.system,
                    upstream_url: drm.license_url.clone(),
                    headers: drm.headers.clone(),
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
/// proxy URLs (`/manifest/{session}/m0.mpd`), and renderers (browser Shaka,
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
        let kind = infer_manifest_kind(Some(&stream.content_type), &stream.url);
        if kind != ManifestKind::Unknown {
            if let Some(base) = manifest_base {
                stream.url = format!(
                    "{base}/m{index}{}?v={media_session_id}",
                    manifest_route_suffix(kind)
                );
            }
        }
        if let Some(drm) = &mut stream.drm {
            if let Some(base) = license_base {
                drm.license_url = format!("{base}?route=r{index}");
                drm.headers = HashMap::new();
            }
        }
    }
}

/// Convert SDK media into the bridge's renderer wire payload.
pub(crate) fn to_payload(media: &PlaybackMedia) -> PlaybackMediaPayload {
    PlaybackMediaPayload {
        streams: media
            .streams
            .iter()
            .map(|stream| PlaybackStreamPayload {
                url: stream.url.clone(),
                content_type: stream.content_type.clone(),
                drm: stream.drm.as_ref().map(|drm| DrmPayload {
                    system: match drm.system {
                        DrmSystem::Widevine => WireDrmSystem::Widevine,
                        DrmSystem::ClearKey => WireDrmSystem::ClearKey,
                    },
                    license_url: drm.license_url.clone(),
                    headers: drm.headers.clone(),
                }),
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
