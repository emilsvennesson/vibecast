//! Async HTTP client for TV4 Play playback resolution.

use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;
use serde_json::{json, Map, Value};
use url::{form_urlencoded, Url};

use crate::models::{Tv4AuthTokenResponse, Tv4GraphqlResponse, Tv4Media, Tv4PlaybackResponse};

const ORIGIN: &str = "https://cast-receiver.a2d.tv";
const REFERER: &str = "https://cast-receiver.a2d.tv/";
const CLIENT_NAME: &str = "nordic-chromecast";
const CLIENT_VERSION: &str = "1.24.0";
const DEFAULT_AUTH_URL: &str = "https://auth.tv4.a2d.tv/v2/auth/token";
const DEFAULT_GRAPHQL_URL: &str = "https://nordic-gateway.tv4.a2d.tv/graphql";
const DEFAULT_PLAYBACK_BASE: &str = "https://playback2.a2d.tv";
const DASH_CONTENT_TYPE: &str = "application/dash+xml";
const HLS_CONTENT_TYPE: &str = "application/x-mpegurl";

const VIDEO_QUERY: &str = r"
      query Video($id: ID!) {
        media(id: $id) {
          ... on Channel { __typename, title, channelType: type, isDrmProtected, images { main16x9 { source }, poster2x3 { source }, logo { source } } }
          ... on Clip { __typename, title, isDrmProtected, images { main16x9 { source } } }
          ... on Movie { __typename, title, isDrmProtected, images { main16x9 { source }, poster2x3 { source } }, synopsis { medium } }
          ... on SportEvent { __typename, title, isDrmProtected, images { main16x9 { source }, poster2x3 { source } }, synopsis { medium } }
          ... on Episode { __typename, title, extendedTitle, isDrmProtected, images { main16x9 { source } }, synopsis { medium }, series { id, title, images { logo { source }, poster2x3 { source } } } }
        }
      }
";

static YOSPACE_ABSOLUTE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"https?://[^\s"'<>]+/csm/builder/[^\s"'<>]+?\.mpd(?:\?[^\s"'<>]+)?"#).unwrap()
});
static YOSPACE_RELATIVE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?P<url>/csm/builder/[^\s"'<>]+?\.mpd(?:\?[^\s"'<>]+)?)"#).unwrap()
});

/// Errors raised while resolving TV4 media.
#[derive(Debug)]
pub enum Tv4Error {
    /// The playback response contained no usable manifest URL.
    NoManifestUrl,
    /// The auth response omitted an access token.
    NoAccessToken,
    /// An upstream HTTP request failed.
    Http(reqwest::Error),
}

impl From<reqwest::Error> for Tv4Error {
    fn from(error: reqwest::Error) -> Self {
        Tv4Error::Http(error)
    }
}

/// A refreshed TV4 auth token pair.
#[derive(Debug, Clone)]
pub struct Tv4AuthTokens {
    pub access_token: String,
    pub refresh_token: String,
}

/// The TV4 API resolution result.
#[derive(Debug, Clone)]
pub struct Tv4ResolvedMedia {
    pub manifest_url: String,
    pub content_type: String,
    pub playback: Tv4PlaybackResponse,
    pub metadata: Option<Tv4Media>,
}

/// Endpoint configuration (overridable for tests).
#[derive(Debug, Clone)]
pub struct Tv4ApiConfig {
    pub auth_url: String,
    pub graphql_url: String,
    pub playback_base: String,
}

impl Default for Tv4ApiConfig {
    fn default() -> Self {
        Self {
            auth_url: DEFAULT_AUTH_URL.to_string(),
            graphql_url: DEFAULT_GRAPHQL_URL.to_string(),
            playback_base: DEFAULT_PLAYBACK_BASE.to_string(),
        }
    }
}

/// Minimal TV4 Play API client.
pub struct Tv4PlayApi {
    client: reqwest::Client,
    config: Tv4ApiConfig,
}

impl Tv4PlayApi {
    /// Build a client with the production endpoints.
    #[must_use]
    pub fn new(client: reqwest::Client) -> Self {
        Self {
            client,
            config: Tv4ApiConfig::default(),
        }
    }

    /// Build a client with custom endpoints (used in tests).
    #[cfg(test)]
    #[must_use]
    pub fn with_config(client: reqwest::Client, config: Tv4ApiConfig) -> Self {
        Self { client, config }
    }

    fn base(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        builder
            .header("Accept", "*/*")
            .header("Accept-Language", "en-US")
            .header("Origin", ORIGIN)
            .header("Referer", REFERER)
            .header("Client-Name", CLIENT_NAME)
    }

    /// Refresh sender-provided TV4 credentials.
    pub async fn refresh_auth(
        &self,
        refresh_token: &str,
        profile_id: Option<&str>,
    ) -> Result<Tv4AuthTokens, Tv4Error> {
        #[derive(Serialize)]
        struct AuthPayload<'a> {
            grant_type: &'a str,
            refresh_token: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            profile_id: Option<&'a str>,
        }

        let payload = AuthPayload {
            grant_type: "refresh_token",
            refresh_token,
            profile_id,
        };
        let response = self
            .base(self.client.post(&self.config.auth_url))
            .json(&payload)
            .send()
            .await?
            .error_for_status()?;
        let token: Tv4AuthTokenResponse = response.json().await?;
        if token.access_token.is_empty() {
            return Err(Tv4Error::NoAccessToken);
        }
        let refresh_token = if token.refresh_token.is_empty() {
            refresh_token.to_string()
        } else {
            token.refresh_token
        };
        Ok(Tv4AuthTokens {
            access_token: token.access_token,
            refresh_token,
        })
    }

    async fn fetch_metadata(
        &self,
        asset_id: &str,
        access_token: Option<&str>,
    ) -> Result<Option<Tv4Media>, Tv4Error> {
        let mut builder = self
            .base(self.client.post(&self.config.graphql_url))
            .header("Client-Version", CLIENT_VERSION);
        if let Some(token) = access_token {
            builder = builder.header("Authorization", format!("Bearer {token}"));
        }
        let response = builder
            .json(&json!({"query": VIDEO_QUERY, "variables": {"id": asset_id}}))
            .send()
            .await?
            .error_for_status()?;
        let payload: Tv4GraphqlResponse = response.json().await?;
        Ok(payload.data.and_then(|data| data.media))
    }

    async fn fetch_playback(
        &self,
        asset_id: &str,
        access_token: Option<&str>,
        custom_data: &Map<String, Value>,
    ) -> Result<Tv4PlaybackResponse, Tv4Error> {
        // Scope the (non-Send) serializer so it is dropped before the await.
        let url = {
            let mut query = form_urlencoded::Serializer::new(String::new());
            query.append_pair("preview", "false");
            query.append_pair("capabilities", "live-drm-adstitch-2,yospace3");
            query.append_pair("service", "tv4play");
            query.append_pair("drm", "widevine");
            query.append_pair("device", "chromecast");
            query.append_pair("protocol", "dash");
            query.append_pair("browser", "GoogleChrome");
            copy_query_param(&mut query, custom_data, "gdpr", "gdpr_consent");
            copy_query_param(&mut query, custom_data, "ifa", "ifa");
            copy_query_param(&mut query, custom_data, "ifaType", "ifa_type");
            copy_query_param(&mut query, custom_data, "orientation", "orientation");
            format!(
                "{}/play/{}?{}",
                self.config.playback_base,
                asset_id,
                query.finish()
            )
        };
        let mut builder = self.base(self.client.get(&url));
        if let Some(token) = access_token {
            builder = builder.header("x-jwt", format!("Bearer {token}"));
        }
        let response = builder.send().await?.error_for_status()?;
        Ok(response.json().await?)
    }

    /// Fetch metadata + playback and select the playable manifest.
    pub async fn resolve_media(
        &self,
        asset_id: &str,
        access_token: Option<&str>,
        custom_data: &Map<String, Value>,
    ) -> Result<Tv4ResolvedMedia, Tv4Error> {
        // Metadata is best-effort; playback resolution proceeds without it.
        let metadata = self
            .fetch_metadata(asset_id, access_token)
            .await
            .ok()
            .flatten();
        let playback = self
            .fetch_playback(asset_id, access_token, custom_data)
            .await?;

        let manifest_url = match &playback.playback_item {
            Some(item) if item.access_url_type.as_deref() == Some("yospace") => {
                match &item.access_url {
                    Some(access_url) => self.resolve_yospace_manifest(access_url).await?,
                    None => String::new(),
                }
            }
            Some(item) => item.manifest_url.clone().unwrap_or_default(),
            None => String::new(),
        };
        if manifest_url.is_empty() {
            return Err(Tv4Error::NoManifestUrl);
        }

        Ok(Tv4ResolvedMedia {
            content_type: content_type_for_manifest(&manifest_url),
            manifest_url,
            playback,
            metadata,
        })
    }

    async fn resolve_yospace_manifest(&self, access_url: &str) -> Result<String, Tv4Error> {
        let response = self
            .base(self.client.get(access_url))
            .send()
            .await?
            .error_for_status()?;
        let base_url = response.url().to_string();
        let body = response.text().await?;
        extract_yospace_builder_url(&body, &base_url).ok_or(Tv4Error::NoManifestUrl)
    }
}

fn copy_query_param(
    query: &mut form_urlencoded::Serializer<'_, String>,
    custom_data: &Map<String, Value>,
    source_key: &str,
    target_key: &str,
) {
    let value = match custom_data.get(source_key) {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Number(n)) => Some(n.to_string()),
        Some(Value::Bool(b)) => Some(b.to_string()),
        _ => None,
    };
    if let Some(value) = value {
        query.append_pair(target_key, &value);
    }
}

fn content_type_for_manifest(url: &str) -> String {
    if url.to_lowercase().contains(".m3u8") {
        HLS_CONTENT_TYPE.to_string()
    } else {
        DASH_CONTENT_TYPE.to_string()
    }
}

fn extract_yospace_builder_url(body: &str, base_url: &str) -> Option<String> {
    let unescaped = html_unescape(body);
    if let Some(matched) = YOSPACE_ABSOLUTE_RE.find(&unescaped) {
        return Some(matched.as_str().to_string());
    }
    let relative = YOSPACE_RELATIVE_RE
        .captures(&unescaped)?
        .name("url")?
        .as_str();
    Some(urljoin(base_url, relative))
}

fn urljoin(base: &str, reference: &str) -> String {
    Url::parse(base)
        .and_then(|base| base.join(reference))
        .map(|url| url.to_string())
        .unwrap_or_else(|_| reference.to_string())
}

/// Minimal HTML entity decode (URLs mostly carry `&amp;`). `&amp;` is decoded
/// last so escaped entities aren't double-decoded.
fn html_unescape(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Merge sender customData layers; later layers override earlier ones.
#[must_use]
pub fn merged_custom_data(layers: &[Option<&Value>]) -> Map<String, Value> {
    let mut merged = Map::new();
    for layer in layers {
        if let Some(Value::Object(object)) = layer {
            for (key, value) in object {
                merged.insert(key.clone(), value.clone());
            }
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yospace_relative_url_is_joined_against_base() {
        let body = r#"<Response><MPD href="/csm/builder/proxy.1,proxy.2.mpd?yo.p.si=abc&amp;ss.sig=sig" /></Response>"#;
        let url = extract_yospace_builder_url(body, "https://yospace.example/access").unwrap();
        assert_eq!(
            url,
            "https://yospace.example/csm/builder/proxy.1,proxy.2.mpd?yo.p.si=abc&ss.sig=sig"
        );
    }

    #[test]
    fn content_type_detects_hls_and_dash() {
        assert_eq!(
            content_type_for_manifest("https://x/a.m3u8"),
            HLS_CONTENT_TYPE
        );
        assert_eq!(
            content_type_for_manifest("https://x/a.mpd"),
            DASH_CONTENT_TYPE
        );
    }
}
