//! Async HTTP client for the DAZN Playback, token-refresh, and legacy license
//! APIs, as used by DAZN's own Chromecast receiver (`@dazn/peng-html5-core`
//! `tv-lite`).

use std::time::{SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine as _;
use serde_json::{json, Value};

use crate::models::{
    MpxLicenseChallenge, MpxLicenseRequest, MpxLicenseResponse, PlaybackResponse, RefreshResponse,
};

const DEFAULT_PLAYBACK_URL: &str = "https://api.playback.indazn.com/v5/Playback";
const DEFAULT_REFRESH_URL: &str = "https://ott-authz-bff-prod.ar.indazn.com/v5/RefreshAccessToken";
/// `@dazn/peng-html5-core` version the Chromecast receiver currently loads.
const APP_VERSION: &str = "0.149.9";
const PLAYER_ID: &str = "@dazn/peng-html5-core/tv-lite/chromecast";
/// Reported device model. DAZN serves its full `tv` ladder (1080p50/60, and
/// UHD where available) only to device models it recognises from its own
/// targeting rules; unknown models get the reduced `tv25f` ladder (720p30).
const MODEL: &str = "Google TV Streamer";
const BRAND: &str = "dazn";
/// A Chromecast (Linux) user agent. DAZN classifies the requesting device from
/// the User-Agent as well as `Model`: an `Android` user agent (as in the
/// receiver's default) is served the mobile `mob25f` ladder capped at 720p30,
/// while a CrKey Linux one gets the full TV ladder (1080p60 and up).
///
/// Some CDN tokens in the Playback response are bound to this User-Agent, so
/// the stream's manifest and segment requests must send it too.
pub const USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux aarch64) AppleWebKit/537.36 (KHTML, like Gecko) \
Chrome/114.0.0.0 Safari/537.36 CrKey/1.56.500000";

/// Errors raised by the DAZN API client.
#[derive(Debug, thiserror::Error)]
pub enum DaznError {
    /// An upstream request failed at the transport layer.
    #[error("DAZN request failed: {0}")]
    Http(reqwest::Error),
    /// An upstream request returned a non-success status.
    #[error("DAZN returned HTTP {0}")]
    Status(u16),
    /// The response lacked a required field.
    #[error("DAZN response missing {0}")]
    MissingField(&'static str),
    /// A response body could not be decoded.
    #[error("DAZN response could not be decoded")]
    Decode,
}

impl From<reqwest::Error> for DaznError {
    fn from(error: reqwest::Error) -> Self {
        match error.status() {
            Some(status) => DaznError::Status(status.as_u16()),
            // Drop the URL: the legacy license URL carries a token.
            None => DaznError::Http(error.without_url()),
        }
    }
}

impl DaznError {
    /// The upstream HTTP status, if any.
    pub fn status(&self) -> Option<u16> {
        match self {
            DaznError::Status(status) => Some(*status),
            DaznError::Http(error) => error.status().map(|status| status.as_u16()),
            DaznError::MissingField(_) | DaznError::Decode => None,
        }
    }

    /// Whether the error means the access token was rejected.
    pub fn is_auth(&self) -> bool {
        matches!(self.status(), Some(401 | 403))
    }
}

/// Stream-quality parameters sent to the Playback API. DAZN chooses the
/// representation ladder server-side from these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QualityParams {
    /// Sorted, comma-joined subset of `4k,dd,ddp,hdr,hevc,mta`.
    pub capabilities: String,
    /// `Some("low")` when only low-security Widevine is available.
    pub drm_security_level: Option<&'static str>,
}

/// One Playback API request.
#[derive(Debug, Clone)]
pub struct PlaybackRequest<'a> {
    pub token: &'a str,
    pub device_guid: &'a str,
    pub asset_id: &'a str,
    pub language: &'a str,
    pub audio_language: Option<&'a str>,
    pub youth_protection_pin: Option<&'a str>,
    pub quality: &'a QualityParams,
}

/// Endpoint configuration (overridable for tests).
#[derive(Debug, Clone)]
pub struct DaznApiConfig {
    pub playback_url: String,
    pub refresh_url: String,
}

impl Default for DaznApiConfig {
    fn default() -> Self {
        Self {
            playback_url: DEFAULT_PLAYBACK_URL.to_string(),
            refresh_url: DEFAULT_REFRESH_URL.to_string(),
        }
    }
}

/// Minimal DAZN API client.
pub struct DaznApi {
    client: reqwest::Client,
    config: DaznApiConfig,
}

impl DaznApi {
    /// Build a client with the production endpoints.
    #[must_use]
    pub fn new(client: reqwest::Client) -> Self {
        Self {
            client,
            config: DaznApiConfig::default(),
        }
    }

    /// Build a client with custom endpoints (used in tests).
    #[cfg(test)]
    #[must_use]
    pub fn with_config(client: reqwest::Client, config: DaznApiConfig) -> Self {
        Self { client, config }
    }

    /// Resolve an asset into CDN candidates, manifest URLs, and license URLs.
    pub async fn playback(
        &self,
        request: &PlaybackRequest<'_>,
    ) -> Result<PlaybackResponse, DaznError> {
        let mut query: Vec<(&str, &str)> = vec![
            ("AppVersion", APP_VERSION),
            ("DrmType", "WIDEVINE"),
            ("Format", "MPEG-DASH"),
            ("PlayerId", PLAYER_ID),
            ("Platform", "chromecast"),
            ("Model", MODEL),
            ("Secure", "true"),
            ("Manufacturer", "Google"),
            ("PlayReadyInitiator", "false"),
            ("Capabilities", &request.quality.capabilities),
            ("AssetId", request.asset_id),
            ("LanguageCode", request.language),
            (
                "MtaLanguageCode",
                request.audio_language.unwrap_or(request.language),
            ),
        ];
        if let Some(level) = request.quality.drm_security_level {
            query.push(("drmSecLvl", level));
        }

        let mut builder = self
            .client
            .get(&self.config.playback_url)
            .query(&query)
            .bearer_auth(request.token)
            .header(reqwest::header::USER_AGENT, USER_AGENT)
            .header("x-dazn-device", request.device_guid)
            .header("x-correlation-id", uuid::Uuid::new_v4().to_string());
        if let Some(pin) = request.youth_protection_pin.filter(|pin| !pin.is_empty()) {
            builder = builder.header("x-age-verification-pin", pin);
        }
        let response = builder.send().await?.error_for_status()?;
        Ok(response.json().await?)
    }

    /// Exchange a (possibly near-expiry) access token for a fresh one.
    pub async fn refresh_token(&self, token: &str, device_guid: &str) -> Result<String, DaznError> {
        let response = self
            .client
            .post(&self.config.refresh_url)
            .bearer_auth(token)
            .header(reqwest::header::USER_AGENT, USER_AGENT)
            .header("x-dazn-device", device_guid)
            .json(&json!({ "DeviceId": device_guid, "brand": BRAND }))
            .send()
            .await?
            .error_for_status()?;
        let body: RefreshResponse = response.json().await?;
        body.auth_token
            .and_then(|auth| auth.token)
            .filter(|token| !token.is_empty())
            .ok_or(DaznError::MissingField("AuthToken.Token"))
    }

    /// Acquire a Widevine license through DAZN's legacy MPX endpoint, which
    /// wraps the challenge and license in JSON.
    pub async fn mpx_widevine_license(
        &self,
        license_url: &str,
        token: &str,
        release_pid: &str,
        challenge: &[u8],
    ) -> Result<Vec<u8>, DaznError> {
        let mpx = jwt_claim(token, "mpx")
            .and_then(|value| value.as_str().map(str::to_string))
            .ok_or(DaznError::MissingField("mpx"))?;
        let url = format!("{license_url}&token={mpx}").replace("/getWidevineLicense", "");
        let body = MpxLicenseRequest {
            get_widevine_license: MpxLicenseChallenge {
                release_pid,
                widevine_challenge: STANDARD.encode(challenge),
            },
        };
        let response = self
            .client
            .post(url)
            .json(&body)
            .send()
            .await?
            .error_for_status()?;
        let body: MpxLicenseResponse = response.json().await?;
        STANDARD
            .decode(body.get_widevine_license_response.license)
            .map_err(|_| DaznError::Decode)
    }
}

/// Decode one claim from a JWT payload without verifying it. The token itself
/// is never logged.
pub fn jwt_claim(token: &str, claim: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    value.get(claim).cloned()
}

/// Whether a JWT expires within `margin_secs` (or has no readable `exp`).
pub fn token_expires_within(token: &str, margin_secs: u64) -> bool {
    let Some(exp) = jwt_claim(token, "exp").and_then(|value| value.as_u64()) else {
        return false;
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    exp <= now.saturating_add(margin_secs)
}

#[cfg(test)]
pub(crate) fn test_jwt(claims: &Value) -> String {
    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#);
    let payload = URL_SAFE_NO_PAD.encode(claims.to_string());
    format!("{header}.{payload}.sig")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_jwt_claims() {
        let token = test_jwt(&json!({"exp": 1, "mpx": "abc"}));
        assert_eq!(jwt_claim(&token, "mpx"), Some(json!("abc")));
        assert_eq!(jwt_claim("not-a-jwt", "mpx"), None);
    }

    #[test]
    fn detects_expiring_tokens() {
        assert!(token_expires_within(&test_jwt(&json!({"exp": 1})), 300));
        let far = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 3600;
        assert!(!token_expires_within(&test_jwt(&json!({"exp": far})), 300));
        assert!(!token_expires_within("opaque-token", 300));
    }
}
