//! Async HTTP client for the Viaplay API.
//!
//! Handles authentication flows (persistent login, token login, device-code
//! authorization) and stream resolution.  Uses the receiver-managed
//! `reqwest::Client` for requests and cookie persistence.

use std::collections::HashMap;

use serde_json::Value;
use vibecast_sdk::StreamType;

use crate::models::{
    ViaplayAuthorizedPollResponse, ViaplayDeviceAuthResponse, ViaplaySessionResponse,
    ViaplayStreamResponse,
};

const ORIGIN: &str = "https://viaplay-chromecast.viaplay.com";
const REFERER: &str = "https://viaplay-chromecast.viaplay.com/";
const DEFAULT_DEVICE_CODE_FALLBACK: &str =
    "https://login.viaplay.com/api/device/code{?deviceKey,deviceId}";

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors raised while resolving Viaplay media.
#[derive(Debug)]
pub enum ViaplayError {
    /// The stream response contained no usable manifest URL.
    NoStreamUrl,
    /// The content root has not been set.
    NoContentRoot,
    /// The device authorization response had no user code.
    NoDeviceCode,
    /// An upstream HTTP request failed.
    Http(reqwest::Error),
    /// JSON deserialization failed.
    Json(serde_json::Error),
    /// An upstream HTTP endpoint returned a non-OK status.
    HttpStatus { status: u16, message: String },
}

impl std::fmt::Display for ViaplayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ViaplayError::NoStreamUrl => write!(f, "no stream URL found in API response"),
            ViaplayError::NoContentRoot => write!(f, "content root not set"),
            ViaplayError::NoDeviceCode => {
                write!(f, "no userCode in device authorization response")
            }
            ViaplayError::Http(e) => write!(f, "{e}"),
            ViaplayError::Json(e) => write!(f, "JSON: {e}"),
            ViaplayError::HttpStatus { status, message } => {
                write!(f, "HTTP {status}: {message}")
            }
        }
    }
}

impl From<reqwest::Error> for ViaplayError {
    fn from(error: reqwest::Error) -> Self {
        ViaplayError::Http(error)
    }
}

impl From<serde_json::Error> for ViaplayError {
    fn from(error: serde_json::Error) -> Self {
        ViaplayError::Json(error)
    }
}

// ---------------------------------------------------------------------------
// Data types returned by the API
// ---------------------------------------------------------------------------

/// Authenticated user information.
#[derive(Debug, Clone)]
pub struct ViaplayUser {
    pub user_id: String,
    pub first_name: String,
    pub last_name: String,
}

/// Result of a content-root session check.
#[derive(Debug, Clone, Default)]
pub struct SessionCheckResult {
    pub user: Option<ViaplayUser>,
    pub persistent_login_url: Option<String>,
    pub token_login_url: Option<String>,
    pub device_auth_url: Option<String>,
}

/// Device-code authorization data.
#[derive(Debug, Clone)]
pub struct DeviceAuthInfo {
    pub user_code: String,
    pub device_token: String,
    pub activate_url: String,
    pub authorized_url: String,
}

/// Resolved stream information.
#[derive(Debug, Clone)]
pub struct StreamInfo {
    pub url: String,
    pub content_type: String,
    pub stream_type: Option<StreamType>,
    pub duration: Option<f64>,
    pub title: Option<String>,
    pub drm_license_url: Option<String>,
    pub fallback_urls: Vec<String>,
}

/// Parameters extracted from the sender's `SETUP_INFO` message.
#[derive(Debug, Clone, Default)]
pub struct SetupParams {
    pub content_root: String,
    pub country_code: String,
    pub user_id: String,
    pub profile_id: String,
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Endpoint configuration (overridable for tests).
#[derive(Debug, Clone)]
pub struct ViaplayApiConfig {
    pub device_code_fallback: String,
}

impl Default for ViaplayApiConfig {
    fn default() -> Self {
        Self {
            device_code_fallback: DEFAULT_DEVICE_CODE_FALLBACK.to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// ViaplayApi
// ---------------------------------------------------------------------------

/// Async HTTP client for the Viaplay API.
#[derive(Clone)]
pub struct ViaplayApi {
    client: reqwest::Client,
    device_id: String,
    user_agent: String,
    config: ViaplayApiConfig,
}

impl ViaplayApi {
    /// Build a client with the production endpoints.
    #[must_use]
    pub fn new(client: reqwest::Client, device_id: String, user_agent: String) -> Self {
        Self {
            client,
            device_id,
            user_agent,
            config: ViaplayApiConfig::default(),
        }
    }

    /// Build a client with custom endpoints (used in tests).
    #[cfg(test)]
    #[must_use]
    pub fn with_config(
        client: reqwest::Client,
        device_id: String,
        user_agent: String,
        config: ViaplayApiConfig,
    ) -> Self {
        Self {
            client,
            device_id,
            user_agent,
            config,
        }
    }

    /// Return default Viaplay headers mimicking a real Chromecast.
    pub fn request_headers(&self) -> HashMap<String, String> {
        let mut headers = HashMap::new();
        headers.insert("Accept".to_string(), "*/*".to_string());
        headers.insert("Accept-Language".to_string(), "en-US".to_string());
        headers.insert("Origin".to_string(), ORIGIN.to_string());
        headers.insert("Referer".to_string(), REFERER.to_string());
        headers
    }

    fn base(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        builder
            .header("Accept", "*/*")
            .header("Accept-Language", "en-US")
            .header("Origin", ORIGIN)
            .header("Referer", REFERER)
    }

    fn template_vars(&self, params: &SetupParams) -> HashMap<String, stduritemplate::Value> {
        let mut vars: HashMap<String, stduritemplate::Value> = HashMap::new();
        let s = |v: &str| stduritemplate::Value::String(v.to_string());
        vars.insert("deviceId".into(), s(&self.device_id));
        vars.insert(
            "deviceKey".into(),
            s(&format!("chromecastgoogletv4k-{}", params.country_code)),
        );
        vars.insert("deviceType".into(), s("chromecast"));
        vars.insert("deviceName".into(), s("chromecast-receiver-v3"));
        vars.insert("userAgent".into(), s(&self.user_agent));
        vars.insert("profileId".into(), s(&params.profile_id));
        vars.insert("cse".into(), s("true"));
        vars
    }

    fn expand(
        &self,
        template: &str,
        params: &SetupParams,
        extra: Option<&HashMap<String, String>>,
    ) -> String {
        let mut vars = self.template_vars(params);
        if let Some(extra) = extra {
            for (k, v) in extra {
                vars.insert(k.clone(), stduritemplate::Value::String(v.clone()));
            }
        }
        stduritemplate::expand(template, &vars).unwrap_or_else(|_| template.to_string())
    }

    /// GET a URL and parse the JSON body.  Does *not* raise on non-200 status.
    async fn get_json(&self, url: &str) -> Result<(Value, u16), ViaplayError> {
        let response = self.base(self.client.get(url)).send().await?;
        let status = response.status().as_u16();
        let body: Value = response.json().await?;
        Ok((body, status))
    }

    // -- authentication methods ----------------------------------------------

    /// Check current session by fetching the content root.
    pub async fn check_session(
        &self,
        params: &SetupParams,
    ) -> Result<SessionCheckResult, ViaplayError> {
        if params.content_root.is_empty() {
            return Err(ViaplayError::NoContentRoot);
        }

        let mut url_template = format!("{}/{{deviceKey}}", params.content_root);
        if !params.profile_id.is_empty() {
            url_template.push_str("?profileId={profileId}");
        }

        let url = self.expand(&url_template, params, None);
        let (body, status) = self.get_json(&url).await?;
        let resp: ViaplaySessionResponse = serde_json::from_value(body)?;

        let user = resp.user.map(|u| ViaplayUser {
            user_id: u.user_id,
            first_name: u.first_name,
            last_name: u.last_name,
        });

        let links = resp.links.unwrap_or_default();
        let result = SessionCheckResult {
            user,
            persistent_login_url: links.persistent_login.map(|l| l.href),
            token_login_url: links.token_login.map(|l| l.href),
            device_auth_url: links.device_authorization.map(|l| l.href),
        };

        if status != 200 {
            tracing::debug!(status, "session check returned non-200");
        } else if result
            .user
            .as_ref()
            .is_some_and(|u| u.user_id == params.user_id)
        {
            tracing::info!(user_id = %params.user_id, "session valid");
        } else {
            tracing::debug!("session check: no matching user");
        }

        Ok(result)
    }

    /// Attempt persistent login at the given URL.  Returns `true` on success.
    pub async fn persistent_login(
        &self,
        url: &str,
        params: &SetupParams,
    ) -> Result<bool, ViaplayError> {
        let expanded = self.expand(url, params, None);
        let (_, status) = self.get_json(&expanded).await?;
        if status == 200 {
            tracing::info!("persistent login succeeded");
            return Ok(true);
        }
        tracing::debug!(status, "persistent login failed");
        Ok(false)
    }

    /// Attempt token login.  Returns `true` on success.
    pub async fn token_login(
        &self,
        url_template: &str,
        access_token: &str,
        params: &SetupParams,
    ) -> Result<bool, ViaplayError> {
        let mut extra = HashMap::new();
        extra.insert("accessToken".to_string(), access_token.to_string());
        let url = self.expand(url_template, params, Some(&extra));
        let response = self.base(self.client.get(&url)).send().await?;
        let status = response.status().as_u16();
        if status == 200 {
            tracing::info!("token login succeeded");
            return Ok(true);
        }
        tracing::debug!(status, "token login failed");
        Ok(false)
    }

    /// Request a device authorization code.
    pub async fn get_device_authorization(
        &self,
        params: &SetupParams,
        root_result: Option<&SessionCheckResult>,
    ) -> Result<DeviceAuthInfo, ViaplayError> {
        let auth_url = root_result
            .and_then(|r| r.device_auth_url.as_deref())
            .unwrap_or(&self.config.device_code_fallback);

        let url = self.expand(auth_url, params, None);
        let (body, status) = self.get_json(&url).await?;
        if status != 200 {
            return Err(ViaplayError::HttpStatus {
                status,
                message: format!("device authorization request failed with status {status}"),
            });
        }

        let resp: ViaplayDeviceAuthResponse = serde_json::from_value(body)?;
        if resp.user_code.is_empty() {
            return Err(ViaplayError::NoDeviceCode);
        }

        let links = resp.links.unwrap_or_default();
        let mut extra = HashMap::new();
        extra.insert("userCode".to_string(), resp.user_code.clone());

        let activate_url = if let Some(link) = &links.activate {
            self.expand(&link.href, params, Some(&extra))
        } else if !resp.verification_url.is_empty() {
            resp.verification_url.clone()
        } else {
            String::new()
        };

        let authorized_url = links.authorized.map(|l| l.href).unwrap_or_default();

        tracing::info!(code = %resp.user_code, "device auth code issued");
        Ok(DeviceAuthInfo {
            user_code: resp.user_code,
            device_token: resp.device_token,
            activate_url,
            authorized_url,
        })
    }

    /// Poll the authorized endpoint.  Returns `true` when the code is activated.
    pub async fn poll_authorized(
        &self,
        auth_info: &DeviceAuthInfo,
        params: &SetupParams,
    ) -> Result<bool, ViaplayError> {
        if auth_info.authorized_url.is_empty() {
            return Ok(false);
        }

        let mut extra = HashMap::new();
        extra.insert("deviceToken".to_string(), auth_info.device_token.clone());
        extra.insert("userCode".to_string(), auth_info.user_code.clone());

        let url = self.expand(&auth_info.authorized_url, params, Some(&extra));
        let (body, status) = self.get_json(&url).await?;

        if status == 200 {
            let resp: ViaplayAuthorizedPollResponse = serde_json::from_value(body)?;
            if let Some(link) = resp.links.and_then(|l| l.persistent_login) {
                let _ = self.persistent_login(&link.href, params).await;
            }
            return Ok(true);
        }
        if status == 403 {
            return Ok(false); // not yet activated
        }
        tracing::debug!(status, "poll authorized returned unexpected status");
        Ok(false)
    }

    // -- stream resolution ---------------------------------------------------

    /// Resolve a play URL to a streaming manifest.
    pub async fn fetch_stream(
        &self,
        play_url: &str,
        params: &SetupParams,
    ) -> Result<StreamInfo, ViaplayError> {
        let resolved_url = self.expand(play_url, params, None);
        let (body, status) = self.get_json(&resolved_url).await?;
        if status != 200 {
            return Err(ViaplayError::HttpStatus {
                status,
                message: format!("stream fetch failed with status {status}"),
            });
        }

        let resp: ViaplayStreamResponse = serde_json::from_value(body)?;
        let stream_type = resolve_stream_type(&resp, play_url);
        let duration = normalize_duration(resp.duration);
        let title = resp
            .product
            .as_ref()
            .map(|p| p.content.title.clone())
            .filter(|t| !t.is_empty());
        let drm_url = extract_drm_url(&resp);
        let fallback_urls = extract_fallbacks(&resp);

        // Path 1: _embedded.viaplay:media.contentUrl
        if let Some(embedded) = &resp.embedded {
            if let Some(media) = &embedded.media {
                if let Some(url) = &media.content_url {
                    if !url.is_empty() {
                        let ct = media
                            .content_type
                            .clone()
                            .filter(|s| !s.is_empty())
                            .unwrap_or_else(|| "application/dash+xml".to_string());
                        return Ok(StreamInfo {
                            url: url.clone(),
                            content_type: ct,
                            stream_type,
                            duration,
                            title,
                            drm_license_url: drm_url,
                            fallback_urls,
                        });
                    }
                }
            }
        }

        // Path 2: top-level contentUrl
        if let Some(url) = &resp.content_url {
            if !url.is_empty() {
                let ct = resp
                    .content_type
                    .clone()
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "application/dash+xml".to_string());
                return Ok(StreamInfo {
                    url: url.clone(),
                    content_type: ct,
                    stream_type,
                    duration,
                    title,
                    drm_license_url: drm_url,
                    fallback_urls,
                });
            }
        }

        // Path 3: _links.viaplay:encryptedPlaylist
        if let Some(links) = &resp.links {
            if let Some(ep) = &links.encrypted_playlist {
                let fmt = if ep.streaming_format.is_empty() {
                    resp.streaming_format.as_deref().unwrap_or("")
                } else {
                    &ep.streaming_format
                };
                let ct = if fmt == "HLS" {
                    "application/x-mpegURL"
                } else {
                    "application/dash+xml"
                };
                return Ok(StreamInfo {
                    url: ep.href.clone(),
                    content_type: ct.to_string(),
                    stream_type,
                    duration,
                    title,
                    drm_license_url: drm_url,
                    fallback_urls,
                });
            }

            // Path 4: _links.viaplay:playlist
            if let Some(pl) = &links.playlist {
                return Ok(StreamInfo {
                    url: pl.href.clone(),
                    content_type: "application/dash+xml".to_string(),
                    stream_type,
                    duration,
                    title,
                    drm_license_url: drm_url,
                    fallback_urls,
                });
            }

            // Path 5: _links.viaplay:stream
            if let Some(st) = &links.stream {
                return Ok(StreamInfo {
                    url: st.href.clone(),
                    content_type: String::new(),
                    stream_type,
                    duration,
                    title,
                    drm_license_url: drm_url,
                    fallback_urls,
                });
            }
        }

        Err(ViaplayError::NoStreamUrl)
    }
}

fn resolve_stream_type(resp: &ViaplayStreamResponse, play_url: &str) -> Option<StreamType> {
    if let Some(product) = &resp.product {
        if !product.stream_type.is_empty() {
            let raw = product.stream_type.to_uppercase();
            if raw == "LIVE" {
                return Some(StreamType::Live);
            }
            if raw == "VOD" || raw == "BUFFERED" {
                return Some(StreamType::Buffered);
            }
        }
    }

    let lowered = play_url.to_lowercase();
    if lowered.contains("bymediaguid") || lowered.contains("play-live.") {
        return Some(StreamType::Live);
    }
    if lowered.contains("byguid") {
        return Some(StreamType::Buffered);
    }

    None
}

fn normalize_duration(raw: f64) -> Option<f64> {
    if raw <= 0.0 {
        return None;
    }
    if raw >= 100_000.0 {
        return Some(raw / 1000.0);
    }
    Some(raw)
}

fn extract_drm_url(resp: &ViaplayStreamResponse) -> Option<String> {
    let links = resp.links.as_ref()?;
    if let Some(wv) = &links.widevine_license {
        return Some(wv.href.clone());
    }
    links.license_link.as_ref().map(|l| l.href.clone())
}

fn extract_fallbacks(resp: &ViaplayStreamResponse) -> Vec<String> {
    resp.links
        .as_ref()
        .map(|l| l.fallback_media.iter().map(|fb| fb.href.clone()).collect())
        .unwrap_or_default()
}
