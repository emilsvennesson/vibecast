//! Async HTTP client for Amazon Prime Video Cast APIs.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use url::{form_urlencoded, Url};
use uuid::Uuid;

use crate::models::{
    AuthRegisterResponse, AuthTokenResponse, LivePlaybackResourcesResponse, PlaybackUrlSetPayload,
    PlayerChromeResourcesResponse, RefreshedEnvelopeResponse, VodPlaybackResourcesResponse,
    WidevineLicenseResponse,
};

const ORIGIN: &str = "https://cloudfront.xp-assets.aiv-cdn.net";
const REFERER: &str = "https://cloudfront.xp-assets.aiv-cdn.net/";
const DEFAULT_AUTH_BASE_URL: &str = "https://api.amazon.co.uk";
const DEFAULT_PLAYBACK_BASE_URL: &str = "https://aby4wfamebrp.api.amazonvideo.com";
const DEFAULT_PLAYBACK_ZAZ_BASE_URL: &str = "https://aby4wfamebrp.zaz.api.amazonvideo.com";

pub const API_DEVICE_TYPE_ID: &str = "A2Y2Z7THWOTN8I";
const API_FIRMWARE_VERSION: &str = "1";
const API_VERSION: &str = "1";

/// Endpoint and device-capability configuration for the Prime API client.
#[derive(Debug, Clone)]
pub struct PrimeApiConfig {
    pub auth_base_url: String,
    pub playback_base_url: String,
    pub playback_zaz_base_url: String,
    pub display_width: u32,
    pub display_height: u32,
    pub hdcp_level: String,
    pub max_video_resolution: String,
    pub supported_codecs: Vec<String>,
    pub dynamic_range_formats: Vec<String>,
    pub supported_frame_rates: Vec<String>,
    pub supported_subtitle_formats: Vec<String>,
}

impl Default for PrimeApiConfig {
    fn default() -> Self {
        Self {
            auth_base_url: DEFAULT_AUTH_BASE_URL.to_string(),
            playback_base_url: DEFAULT_PLAYBACK_BASE_URL.to_string(),
            playback_zaz_base_url: DEFAULT_PLAYBACK_ZAZ_BASE_URL.to_string(),
            display_width: 1920,
            display_height: 1080,
            hdcp_level: "1.4".to_string(),
            max_video_resolution: "1080p".to_string(),
            supported_codecs: vec!["H265".to_string(), "H264".to_string()],
            dynamic_range_formats: vec!["None".to_string()],
            supported_frame_rates: vec!["Standard".to_string(), "High".to_string()],
            supported_subtitle_formats: vec!["TTMLv2".to_string(), "DFXP".to_string()],
        }
    }
}

/// Errors raised while calling Prime APIs.
#[derive(Debug)]
pub enum PrimeError {
    NoRefreshToken,
    NoActorToken,
    NoPlaybackUrls,
    NoPlaybackEnvelope,
    NoWidevineLicense,
    Http(reqwest::Error),
    Json(serde_json::Error),
    HttpStatus { status: u16, message: String },
}

impl std::fmt::Display for PrimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoRefreshToken => write!(f, "prime register returned no refresh token"),
            Self::NoActorToken => write!(f, "prime token exchange returned no actor token"),
            Self::NoPlaybackUrls => write!(f, "prime playback response had no manifest URLs"),
            Self::NoPlaybackEnvelope => write!(f, "prime envelope refresh returned no envelope"),
            Self::NoWidevineLicense => write!(f, "prime license response missing license"),
            Self::Http(error) => write!(f, "{error}"),
            Self::Json(error) => write!(f, "JSON: {error}"),
            Self::HttpStatus { status, message } => write!(f, "HTTP {status}: {message}"),
        }
    }
}

impl std::error::Error for PrimeError {}

impl From<reqwest::Error> for PrimeError {
    fn from(error: reqwest::Error) -> Self {
        Self::Http(error)
    }
}

impl From<serde_json::Error> for PrimeError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

/// Auth token tuple returned by Prime registration/token exchange.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrimeAuthTokens {
    pub account_refresh_token: String,
    pub actor_access_token: String,
    pub actor_refresh_token: Option<String>,
}

/// Refreshed playback envelope fields for one title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrimeEnvelopeData {
    pub playback_envelope: String,
    pub correlation_id: Option<String>,
}

/// Human-readable metadata for one Prime title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrimeCatalogMetadata {
    pub title: Option<String>,
    pub subtitle: Option<String>,
}

/// Normalized playback-resource response.
#[derive(Debug, Clone, Default)]
pub struct PrimePlaybackResources {
    pub session_handoff_token: Option<String>,
    pub default_url_set_id: Option<String>,
    pub url_sets: Vec<PlaybackUrlSetPayload>,
}

/// Inputs for one Prime Widevine license request.
pub struct WidevineLicenseParams<'a> {
    pub token: &'a str,
    pub device_id: &'a str,
    pub marketplace_id: &'a str,
    pub title_id: &'a str,
    pub playback_envelope: &'a str,
    pub session_handoff_token: Option<&'a str>,
    pub challenge: &'a [u8],
    pub locale: &'a str,
    pub is_live: bool,
}

/// Minimal Prime Video API client.
#[derive(Clone)]
pub struct PrimeVideoApi {
    client: reqwest::Client,
    config: PrimeApiConfig,
}

impl PrimeVideoApi {
    /// Build a Prime API client.
    #[must_use]
    pub fn new(client: reqwest::Client, config: PrimeApiConfig) -> Self {
        Self { client, config }
    }

    /// Build the Widevine license endpoint URL for one title.
    #[must_use]
    pub fn widevine_license_url(
        &self,
        device_id: &str,
        marketplace_id: &str,
        title_id: &str,
        locale: &str,
        is_live: bool,
    ) -> String {
        let path = if is_live {
            "/playback/drm/GetWidevineLicense"
        } else {
            "/playback/drm-vod/GetWidevineLicense"
        };
        self.build_playback_zaz_url(
            path,
            &[
                ("deviceID", device_id.to_string()),
                ("deviceTypeID", API_DEVICE_TYPE_ID.to_string()),
                ("gascEnabled", "true".to_string()),
                ("marketplaceID", marketplace_id.to_string()),
                ("uxLocale", locale.to_string()),
                ("firmware", API_FIRMWARE_VERSION.to_string()),
                ("titleId", title_id.to_string()),
                ("nerid", new_nerid()),
            ],
        )
    }

    /// Run Prime's device registration API using a sender-provided link code.
    pub async fn register_device(
        &self,
        link_code: &str,
        device_id: &str,
    ) -> Result<PrimeAuthTokens, PrimeError> {
        let url = format!("{}/auth/register", self.config.auth_base_url);
        let payload = json!({
            "registration_data": {
                "device_serial": device_id,
                "os_version": "Android",
                "app_name": "Prime Video",
                "app_version": "1.0",
                "device_model": "Generic GCast",
                "device_name": format!("GCast:{API_DEVICE_TYPE_ID}.{device_id}"),
                "device_type": API_DEVICE_TYPE_ID,
                "domain": "Device",
                "software_version": "1.0",
            },
            "auth_data": {"code": link_code},
            "requested_token_type": ["bearer"],
            "scopes": ["aiv:full"],
        });
        let register: AuthRegisterResponse = self
            .post_json(&url, &payload, None, "application/json")
            .await?;
        let refresh_token = register
            .response
            .and_then(|r| r.success)
            .and_then(|s| s.tokens)
            .and_then(|t| t.bearer)
            .map(|b| b.refresh_token)
            .filter(|token| !token.is_empty())
            .ok_or(PrimeError::NoRefreshToken)?;

        Ok(PrimeAuthTokens {
            account_refresh_token: refresh_token,
            actor_access_token: String::new(),
            actor_refresh_token: None,
        })
    }

    /// Exchange an account refresh token for an actor access token.
    pub async fn exchange_actor_token(
        &self,
        actor_id: &str,
        account_refresh_token: &str,
    ) -> Result<PrimeAuthTokens, PrimeError> {
        let url = format!("{}/auth/token", self.config.auth_base_url);
        let payload = json!({
            "actor_id": actor_id,
            "app_name": "Prime Video",
            "requested_token_type": "actor_access_token",
            "source_token_type": "refresh_token",
            "source_device_tokens": [{
                "account_refresh_token": {"token": account_refresh_token},
                "device_type": API_DEVICE_TYPE_ID,
            }],
        });
        let tokens: AuthTokenResponse = self
            .post_json(&url, &payload, None, "application/json")
            .await?;
        let device_token = tokens
            .device_tokens
            .into_iter()
            .next()
            .ok_or(PrimeError::NoActorToken)?;
        let actor_access_token = device_token
            .actor_access_token
            .map(|t| t.token)
            .filter(|token| !token.is_empty())
            .ok_or(PrimeError::NoActorToken)?;
        let actor_refresh_token = device_token
            .actor_refresh_token
            .map(|t| t.token)
            .filter(|token| !token.is_empty());

        Ok(PrimeAuthTokens {
            account_refresh_token: account_refresh_token.to_string(),
            actor_access_token,
            actor_refresh_token,
        })
    }

    /// Refresh a playback envelope using the sender's correlation id.
    pub async fn refresh_playback_envelope(
        &self,
        token: &str,
        device_id: &str,
        marketplace_id: &str,
        title_id: &str,
        correlation_id: &str,
    ) -> Result<PrimeEnvelopeData, PrimeError> {
        let url = self.build_playback_url(
            "/playback/tags/getRefreshedPlaybackEnvelope",
            &[
                ("deviceID", device_id.to_string()),
                ("deviceTypeID", API_DEVICE_TYPE_ID.to_string()),
                ("gascEnabled", "true".to_string()),
                ("marketplaceID", marketplace_id.to_string()),
                ("firmware", API_FIRMWARE_VERSION.to_string()),
                ("version", API_VERSION.to_string()),
                ("nerid", new_nerid()),
            ],
        );
        let payload = json!({
            "deviceId": device_id,
            "deviceTypeId": API_DEVICE_TYPE_ID,
            "identifiers": {title_id: correlation_id},
            "geoToken": Value::Null,
            "identityContext": Value::Null,
        });
        let refreshed: RefreshedEnvelopeResponse = self
            .post_json(&url, &payload, Some(token), "text/plain")
            .await?;
        let experience = refreshed
            .response
            .get(title_id)
            .and_then(|item| item.playback_experience.as_ref())
            .ok_or(PrimeError::NoPlaybackEnvelope)?;
        let playback_envelope = experience
            .playback_envelope
            .clone()
            .filter(|envelope| !envelope.is_empty())
            .ok_or(PrimeError::NoPlaybackEnvelope)?;
        Ok(PrimeEnvelopeData {
            playback_envelope,
            correlation_id: experience.correlation_id.clone(),
        })
    }

    /// Resolve a VOD title to DASH URL sets and sessionization data.
    pub async fn get_vod_playback_resources(
        &self,
        token: &str,
        device_id: &str,
        marketplace_id: &str,
        title_id: &str,
        playback_envelope: &str,
        locale: &str,
    ) -> Result<PrimePlaybackResources, PrimeError> {
        let url = self.playback_resources_url(
            "/playback/prs/GetVodPlaybackResources",
            device_id,
            marketplace_id,
            title_id,
            locale,
        );
        let payload = self.build_vod_playback_request(title_id, playback_envelope);
        let response: VodPlaybackResourcesResponse = self
            .post_json(&url, &payload, Some(token), "text/plain")
            .await?;
        vod_resources(response)
    }

    /// Resolve a live title to DASH URL sets and sessionization data.
    pub async fn get_live_playback_resources(
        &self,
        token: &str,
        device_id: &str,
        marketplace_id: &str,
        title_id: &str,
        playback_envelope: &str,
        locale: &str,
    ) -> Result<PrimePlaybackResources, PrimeError> {
        let url = self.playback_resources_url(
            "/playback/prs/GetLivePlaybackResources",
            device_id,
            marketplace_id,
            title_id,
            locale,
        );
        let payload = self.build_live_playback_request(title_id, playback_envelope, locale);
        let response: LivePlaybackResourcesResponse = self
            .post_json(&url, &payload, Some(token), "text/plain")
            .await?;
        live_resources(response)
    }

    /// Fetch display metadata for a Prime title.
    pub async fn get_catalog_metadata(
        &self,
        token: &str,
        device_id: &str,
        marketplace_id: &str,
        title_id: &str,
        locale: &str,
    ) -> Result<Option<PrimeCatalogMetadata>, PrimeError> {
        let url = self.build_playback_url(
            "/cdp/lumina/playerChromeResources/v1",
            &[
                ("deviceID", device_id.to_string()),
                ("deviceTypeID", API_DEVICE_TYPE_ID.to_string()),
                ("gascEnabled", "true".to_string()),
                ("marketplaceID", marketplace_id.to_string()),
                ("uxLocale", locale.to_string()),
                ("desiredResources", "catalogMetadataV2".to_string()),
                ("entityId", title_id.to_string()),
                ("firmware", API_FIRMWARE_VERSION.to_string()),
                ("widgetScheme", "pvplayer-web-v2".to_string()),
                ("nerid", new_nerid()),
            ],
        );
        let parsed: PlayerChromeResourcesResponse = self.get_json(&url, Some(token)).await?;
        let Some(catalog) = parsed
            .resources
            .and_then(|r| r.catalog_metadata_v2)
            .and_then(|section| section.catalog)
        else {
            return Ok(None);
        };

        let title = non_empty(catalog.event_title).or_else(|| non_empty(catalog.title));
        let subtitle = non_empty(catalog.series_title);
        if title.is_none() && subtitle.is_none() {
            return Ok(None);
        }
        Ok(Some(PrimeCatalogMetadata { title, subtitle }))
    }

    /// Resolve one Widevine license challenge through Prime's DRM endpoint.
    pub async fn get_widevine_license(
        &self,
        params: WidevineLicenseParams<'_>,
    ) -> Result<Vec<u8>, PrimeError> {
        let url = self.widevine_license_url(
            params.device_id,
            params.marketplace_id,
            params.title_id,
            params.locale,
            params.is_live,
        );
        let mut payload = json!({
            "includeHdcpTestKey": true,
            "playbackEnvelope": params.playback_envelope,
            "licenseChallenge": BASE64.encode(params.challenge),
        });
        if let (Some(token), Some(object)) = (params.session_handoff_token, payload.as_object_mut())
        {
            object.insert(
                "sessionHandoffToken".to_string(),
                Value::String(token.to_string()),
            );
        }

        let parsed: WidevineLicenseResponse = self
            .post_json(&url, &payload, Some(params.token), "text/plain")
            .await?;
        let license = parsed
            .widevine_license
            .map(|payload| payload.license)
            .filter(|license| !license.is_empty())
            .ok_or(PrimeError::NoWidevineLicense)?;
        decode_b64(&license).map_err(|_| PrimeError::NoWidevineLicense)
    }

    /// Ensure Prime's Chromecast device type query parameter is present.
    #[must_use]
    pub fn with_device_type_query(&self, url: &str) -> String {
        let Ok(mut parsed) = Url::parse(url) else {
            return url.to_string();
        };
        let has_device_type = parsed.query_pairs().any(|(key, _)| key == "amznDtid");
        if !has_device_type {
            parsed
                .query_pairs_mut()
                .append_pair("amznDtid", API_DEVICE_TYPE_ID);
        }
        parsed.to_string()
    }

    fn playback_resources_url(
        &self,
        path: &str,
        device_id: &str,
        marketplace_id: &str,
        title_id: &str,
        locale: &str,
    ) -> String {
        self.build_playback_zaz_url(
            path,
            &[
                ("deviceID", device_id.to_string()),
                ("deviceTypeID", API_DEVICE_TYPE_ID.to_string()),
                ("gascEnabled", "true".to_string()),
                ("marketplaceID", marketplace_id.to_string()),
                ("uxLocale", locale.to_string()),
                ("firmware", API_FIRMWARE_VERSION.to_string()),
                ("titleId", title_id.to_string()),
                ("nerid", new_nerid()),
            ],
        )
    }

    fn build_vod_playback_request(&self, title_id: &str, playback_envelope: &str) -> Value {
        json!({
            "globalParameters": self.global_parameters(playback_envelope, false),
            "auditPingsRequest": {},
            "widevineServiceCertificateRequest": {},
            "playbackDataRequest": {},
            "timedTextUrlsRequest": {
                "supportedTimedTextFormats": self.config.supported_subtitle_formats,
            },
            "trickplayUrlsRequest": {},
            "transitionTimecodesRequest": {},
            "vodPlaybackUrlsRequest": {
                "device": {
                    "hdcpLevel": self.config.hdcp_level,
                    "maxVideoResolution": self.config.max_video_resolution,
                    "supportedStreamingTechnologies": ["DASH"],
                    "streamingTechnologies": {"DASH": self.dash_capabilities(true)},
                    "displayWidth": self.config.display_width,
                    "displayHeight": self.config.display_height,
                },
                "ads": {
                    "sitePageUrl": "https://cloudfront.xp-assets.aiv-cdn.net/packages/ATVGCastReceiver-1.0/prod/index.html",
                    "gdpr": {"enabled": false, "consentMap": {}},
                    "mainContentResumeOffsetHintMillis": 0,
                },
                "playbackCustomizations": {},
                "playbackSettingsRequest": playback_settings(title_id),
            },
            "vodXrayMetadataRequest": {
                "xrayDeviceClass": "normal",
                "xrayPlaybackMode": "playback",
                "xrayToken": "XRAY_WEB_2023_V2",
            },
        })
    }

    fn build_live_playback_request(
        &self,
        title_id: &str,
        playback_envelope: &str,
        locale: &str,
    ) -> Value {
        json!({
            "globalParameters": self.global_parameters(playback_envelope, true),
            "auditPingsRequest": {},
            "widevineServiceCertificateRequest": {},
            "playbackDataRequest": {},
            "livePlaybackUrlsRequest": {
                "ads": {"gdpr": {"enabled": false, "consentMap": {}}},
                "device": {
                    "firmwareVersion": "1.56.500000",
                    "hdcpLevel": self.config.hdcp_level,
                    "liveManifestTypes": ["PatternTemplate", "Live"],
                    "playableLiveManifestTypes": {
                        "PatternTemplate": {
                            "daiSettings": {
                                "supportsDai": "notSupported",
                                "supportedDaiFeatures": {"supportsEmbeddedTrickplay": "notSupported"},
                            },
                            "embeddedTrickplaySettings": {"supportsEmbeddedTrickplay": "notSupported"},
                        },
                        "Live": {
                            "daiSettings": {
                                "supportsDai": "supported",
                                "supportedDaiFeatures": {"supportsEmbeddedTrickplay": "notSupported"},
                            },
                            "embeddedTrickplaySettings": {"supportsEmbeddedTrickplay": "notSupported"},
                        },
                    },
                    "maxVideoResolution": self.config.max_video_resolution,
                    "operatingSystem": "Android",
                    "supportedStreamingTechnologies": ["DASH"],
                    "streamingTechnologies": {"DASH": self.dash_capabilities(false)},
                },
                "playbackSettingsRequest": playback_settings(title_id),
            },
            "xrayMetadataRequest": {
                "preferredLocale": locale,
                "xrayDeviceClass": "normal",
                "xrayPlaybackMode": "playback",
                "xrayToken": "XRAY_WEB_2023_V2",
            },
        })
    }

    fn global_parameters(&self, playback_envelope: &str, live: bool) -> Value {
        let mut value = json!({
            "deviceCapabilityFamily": "WebPlayer",
            "playbackEnvelope": playback_envelope,
            "capabilityDiscriminators": {
                "operatingSystem": {"name": "Android", "version": "11.0"},
                "deviceModel": {"name": "SHIELD Android TV", "version": "UNKNOWN"},
                "middleware": {"name": "Chrome", "version": "92.0.4515.0"},
                "nativeApplication": {"name": "CAF Receiver SDK", "version": "3.0.0137"},
                "firmware": {"name": "UNKNOWN", "version": "1.56.500000"},
                "hfrControlMode": "Legacy",
                "displayResolution": {
                    "height": self.config.display_height,
                    "width": self.config.display_width,
                },
            },
        });
        if live {
            if let Some(object) = value.as_object_mut() {
                object.insert(
                    "sessionTrackingMode".to_string(),
                    Value::String("WITH_SESSION_HANDOFF".to_string()),
                );
                object.insert(
                    "userWatchSessionId".to_string(),
                    Value::String(Uuid::new_v4().to_string()),
                );
            }
        }
        value
    }

    fn dash_capabilities(&self, vod: bool) -> Value {
        let mut value = json!({
            "bitrateAdaptations": ["CBR", "CVBR"],
            "codecs": self.config.supported_codecs,
            "drmKeyScheme": "DualKey",
            "drmType": "Widevine",
            "dynamicRangeFormats": self.config.dynamic_range_formats,
            "edgeDeliveryAuthorizationSchemes": ["PVExchangeV1", "Transparent"],
            "fragmentRepresentations": ["ByteOffsetRange", "SeparateFile"],
            "frameRates": self.config.supported_frame_rates,
        });
        if vod {
            if let Some(object) = value.as_object_mut() {
                object.insert(
                    "stitchType".to_string(),
                    Value::String("MultiPeriod".to_string()),
                );
                object.insert(
                    "segmentInfoType".to_string(),
                    Value::String("Base".to_string()),
                );
                object.insert(
                    "timedTextRepresentations".to_string(),
                    json!(["NotInManifestNorStream", "SeparateStreamInManifest"]),
                );
                object.insert(
                    "trickplayRepresentations".to_string(),
                    json!(["NotInManifestNorStream"]),
                );
                object.insert(
                    "variableAspectRatio".to_string(),
                    Value::String("unsupported".to_string()),
                );
            }
        }
        value
    }

    fn build_playback_url(&self, path: &str, query: &[(&str, String)]) -> String {
        build_url(&self.config.playback_base_url, path, query)
    }

    fn build_playback_zaz_url(&self, path: &str, query: &[(&str, String)]) -> String {
        build_url(&self.config.playback_zaz_base_url, path, query)
    }

    async fn get_json<T>(&self, url: &str, token: Option<&str>) -> Result<T, PrimeError>
    where
        T: DeserializeOwned,
    {
        let response = self
            .headers(self.client.get(url), token, None)
            .send()
            .await?;
        parse_json(response).await
    }

    async fn post_json<T>(
        &self,
        url: &str,
        payload: &Value,
        token: Option<&str>,
        content_type: &str,
    ) -> Result<T, PrimeError>
    where
        T: DeserializeOwned,
    {
        let builder = self.headers(self.client.post(url), token, Some(content_type));
        let builder = if content_type == "application/json" {
            builder.json(payload)
        } else {
            builder.body(serde_json::to_string(payload)?)
        };
        let response = builder.send().await?;
        parse_json(response).await
    }

    fn headers(
        &self,
        builder: reqwest::RequestBuilder,
        token: Option<&str>,
        content_type: Option<&str>,
    ) -> reqwest::RequestBuilder {
        let mut builder = builder
            .header("Accept", "*/*")
            .header("Accept-Language", "en-US")
            .header("Origin", ORIGIN)
            .header("Referer", REFERER);
        if let Some(content_type) = content_type {
            builder = builder.header("Content-Type", content_type);
        }
        if let Some(token) = token.filter(|token| !token.is_empty()) {
            builder = builder.header("Authorization", format!("Bearer {token}"));
        }
        builder
    }
}

async fn parse_json<T>(response: reqwest::Response) -> Result<T, PrimeError>
where
    T: DeserializeOwned,
{
    let status = response.status();
    if status.as_u16() >= 400 {
        let preview = response
            .text()
            .await
            .unwrap_or_default()
            .replace('\n', " ")
            .chars()
            .take(300)
            .collect();
        return Err(PrimeError::HttpStatus {
            status: status.as_u16(),
            message: preview,
        });
    }
    Ok(response.json::<T>().await?)
}

fn vod_resources(
    response: VodPlaybackResourcesResponse,
) -> Result<PrimePlaybackResources, PrimeError> {
    let playback_urls = response
        .vod_playback_urls
        .and_then(|section| section.result)
        .and_then(|result| result.playback_urls)
        .ok_or(PrimeError::NoPlaybackUrls)?;
    Ok(PrimePlaybackResources {
        session_handoff_token: response
            .sessionization
            .and_then(|session| session.session_handoff_token),
        default_url_set_id: playback_urls.default_url_set_id,
        url_sets: playback_urls.url_sets,
    })
}

fn live_resources(
    response: LivePlaybackResourcesResponse,
) -> Result<PrimePlaybackResources, PrimeError> {
    let result = response
        .live_playback_urls
        .and_then(|section| section.result)
        .ok_or(PrimeError::NoPlaybackUrls)?;
    let url_sets: Vec<PlaybackUrlSetPayload> = result
        .url_sets
        .into_iter()
        .filter_map(|url_set| {
            let url = url_set.urls?.manifest?.url?;
            if url.is_empty() {
                return None;
            }
            Some(PlaybackUrlSetPayload {
                url_set_id: url_set.url_set_id,
                url,
            })
        })
        .collect();
    if url_sets.is_empty() {
        return Err(PrimeError::NoPlaybackUrls);
    }
    Ok(PrimePlaybackResources {
        session_handoff_token: response
            .sessionization
            .and_then(|session| session.session_handoff_token),
        default_url_set_id: result.default_url_set_id,
        url_sets,
    })
}

fn playback_settings(title_id: &str) -> Value {
    json!({
        "deviceModel": "SHIELD Android TV",
        "firmware": "1.56.500000",
        "playerType": "xp",
        "responseFormatVersion": "1.0.0",
        "titleId": title_id,
    })
}

fn build_url(base: &str, path: &str, query: &[(&str, String)]) -> String {
    let encoded = query
        .iter()
        .fold(
            form_urlencoded::Serializer::new(String::new()),
            |mut serializer, (key, value)| {
                serializer.append_pair(key, value);
                serializer
            },
        )
        .finish();
    format!("{base}{path}?{encoded}")
}

fn new_nerid() -> String {
    let uuid = Uuid::new_v4().simple().to_string();
    format!("vibecast{}", &uuid[..16])
}

fn non_empty(value: Option<String>) -> Option<String> {
    let trimmed = value?.trim().to_string();
    (!trimmed.is_empty()).then_some(trimmed)
}

fn decode_b64(value: &str) -> Result<Vec<u8>, base64::DecodeError> {
    let mut padded = value.to_string();
    let missing = (4 - padded.len() % 4) % 4;
    padded.extend(std::iter::repeat_n('=', missing));
    BASE64.decode(padded)
}
