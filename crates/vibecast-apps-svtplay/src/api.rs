//! Async HTTP client that resolves SVT Play media manifests.

use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;
use serde::de::DeserializeOwned;
use serde_json::{json, Map, Value};
use url::form_urlencoded;
use vibecast_sdk::{DrmInfo, DrmSystem};

use crate::models::{SvtResolveResponse, SvtVideoReference, SvtVideoResponse};

const ORIGIN: &str = "https://www.svtstatic.se";
const REFERER: &str = "https://www.svtstatic.se/";
const DEFAULT_VIDEO_BASE: &str = "https://video.svt.se/video";
const DEFAULT_DITTO_ENDPOINT: &str = "https://api.svt.se/ditto/api/v3/manifest";
const PLATFORM: &str = "chromecast;cc-androidtv";
const AUDIO_CODECS: &str = "mp4a.40.2";
const VIDEO_CODECS: &str = "hvc1.2.4.L123.90,hvc1.1.6.L123.90,avc1.64002a,avc1.640029,avc1.640020,avc1.64001f,avc1.4d401f,avc1.42c01f,avc1.42c015";
const BUILD_PARAM: &str = "-6334";
const DASH_MIME_TYPE: &str = "application/dash+xml";
const CLEARKEY_SCHEME_URI: &str = "urn:uuid:e2719d58-a985-b3c9-781a-b030af78d30e";
const WIDEVINE_SCHEME_URI: &str = "urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed";

static DASHIF_LAURL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)<dashif:Laurl[^>]*>([^<]+)</dashif:Laurl>").unwrap());
static MS_LAURL_ATTR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)<ms:laurl[^>]*(?:licenseUrl|href)="([^"]+)"[^>]*?/?>"#).unwrap()
});
static MS_LAURL_TEXT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)<ms:laurl[^>]*>([^<]+)</ms:laurl>").unwrap());

/// Errors raised while resolving SVT media.
#[derive(Debug)]
pub enum SvtError {
    /// No usable DASH reference was found.
    NoDashReference,
    /// No playable streams were produced.
    NoResolvedStreams,
    /// An upstream HTTP request failed.
    Http(reqwest::Error),
}

impl From<reqwest::Error> for SvtError {
    fn from(error: reqwest::Error) -> Self {
        SvtError::Http(error)
    }
}

/// Endpoint configuration (overridable for tests).
#[derive(Debug, Clone)]
pub struct SvtApiConfig {
    /// Base URL for `GET {video_base}/{svt_id}`.
    pub video_base: String,
    /// Ditto manifest endpoint.
    pub ditto_endpoint: String,
}

impl Default for SvtApiConfig {
    fn default() -> Self {
        Self {
            video_base: DEFAULT_VIDEO_BASE.to_string(),
            ditto_endpoint: DEFAULT_DITTO_ENDPOINT.to_string(),
        }
    }
}

/// A single resolved stream candidate.
#[derive(Debug, Clone)]
pub struct SvtResolvedStream {
    /// Stream URL.
    pub url: String,
    /// MIME type.
    pub content_type: String,
    /// Detected DRM, if any.
    pub drm: Option<DrmInfo>,
}

/// Resolved playback payload for one SVT content item.
#[derive(Debug, Clone)]
pub struct SvtResolvedMedia {
    /// Stream candidates in preference order.
    pub streams: Vec<SvtResolvedStream>,
    /// Program title.
    pub title: Option<String>,
    /// Episode title.
    pub subtitle: Option<String>,
    /// Content duration.
    pub duration: Option<f64>,
    /// Echoed custom data (with an added `response` summary).
    pub custom_data: Value,
}

/// Minimal SVT Play API client.
pub struct SvtPlayApi {
    client: reqwest::Client,
    config: SvtApiConfig,
}

impl SvtPlayApi {
    /// Build a client with the production endpoints.
    #[must_use]
    pub fn new(client: reqwest::Client) -> Self {
        Self {
            client,
            config: SvtApiConfig::default(),
        }
    }

    /// Build a client with custom endpoints (used in tests).
    #[cfg(test)]
    #[must_use]
    pub fn with_config(client: reqwest::Client, config: SvtApiConfig) -> Self {
        Self { client, config }
    }

    fn request(&self, url: &str) -> reqwest::RequestBuilder {
        self.client
            .get(url)
            .header("Accept", "*/*")
            .header("Accept-Language", "en-US")
            .header("Origin", ORIGIN)
            .header("Referer", REFERER)
    }

    async fn get_json<T: DeserializeOwned>(&self, url: &str) -> Result<T, SvtError> {
        let response = self.request(url).send().await?.error_for_status()?;
        Ok(response.json::<T>().await?)
    }

    async fn get_text(&self, url: &str) -> Result<String, SvtError> {
        let response = self.request(url).send().await?.error_for_status()?;
        Ok(response.text().await?)
    }

    async fn fetch_video(&self, svt_id: &str) -> Result<SvtVideoResponse, SvtError> {
        self.get_json(&format!("{}/{}", self.config.video_base, svt_id))
            .await
    }

    /// Resolve one SVT content id into ordered stream candidates.
    pub async fn resolve_media(
        &self,
        svt_id: &str,
        media_custom_data: Option<&Value>,
    ) -> Result<SvtResolvedMedia, SvtError> {
        let video = self.fetch_video(svt_id).await?;

        let reference_pool: Vec<SvtVideoReference> =
            match video.variants.get("default").and_then(Option::as_ref) {
                Some(default) => default.video_references.clone(),
                None => video.video_references.clone(),
            };

        let primary_ref = pick_dash_reference(&reference_pool)
            .or_else(|| pick_dash_reference(&video.video_references))
            .cloned()
            .ok_or(SvtError::NoDashReference)?;
        let primary_manifest = self.resolve_reference(&primary_ref).await?;

        let mut sign_manifest: Option<String> = None;
        if let Some(sign_ref) = variant_dash_reference(&video, "signInterpreted") {
            sign_manifest = Some(self.resolve_reference(&sign_ref).await?);
        }
        let mut audio_manifest: Option<String> = None;
        if let Some(audio_ref) = variant_dash_reference(&video, "audioDescribed") {
            audio_manifest = Some(self.resolve_reference(&audio_ref).await?);
        }

        // Scope the (non-Send) serializer so it is dropped before any await.
        let ditto_url = {
            let mut query = form_urlencoded::Serializer::new(String::new());
            query.append_pair("manifestUrl", &primary_manifest);
            query.append_pair("platform", PLATFORM);
            query.append_pair("includeAudioCodecs", AUDIO_CODECS);
            query.append_pair("includeVideoCodecs", VIDEO_CODECS);
            if let Some(sign) = &sign_manifest {
                query.append_pair("manifestUrlSignLanguage", sign);
            }
            if let Some(audio) = &audio_manifest {
                query.append_pair("manifestUrlAudioDescription", audio);
            }
            if let Some(track) = preferred_video_track(media_custom_data) {
                if sign_manifest.is_some() || audio_manifest.is_some() {
                    query.append_pair("preferredVideoTrack", &track);
                }
            }
            query.append_pair("b", BUILD_PARAM);
            format!("{}?{}", self.config.ditto_endpoint, query.finish())
        };

        let mut streams: Vec<SvtResolvedStream> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        self.add_stream(&mut streams, &mut seen, ditto_url).await;
        self.add_stream(&mut streams, &mut seen, primary_manifest.clone())
            .await;
        for fallback_ref in pick_dash_fallback_references(&reference_pool) {
            let fallback_manifest = self.resolve_reference(&fallback_ref).await?;
            self.add_stream(&mut streams, &mut seen, fallback_manifest)
                .await;
        }

        if streams.is_empty() {
            return Err(SvtError::NoResolvedStreams);
        }

        let mut custom = media_custom_data
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_else(Map::new);
        custom.entry("response".to_string()).or_insert_with(|| {
            json!({
                "svtId": video.svt_id,
                "programTitle": video.program_title,
                "episodeTitle": video.episode_title,
                "contentDuration": video.content_duration,
            })
        });

        Ok(SvtResolvedMedia {
            streams,
            title: video.program_title,
            subtitle: video.episode_title,
            duration: video.content_duration,
            custom_data: Value::Object(custom),
        })
    }

    async fn resolve_reference(&self, reference: &SvtVideoReference) -> Result<String, SvtError> {
        match &reference.resolve {
            Some(resolve_url) => {
                let resolved: SvtResolveResponse = self.get_json(resolve_url).await?;
                Ok(resolved.location)
            }
            None => Ok(reference.url.clone()),
        }
    }

    async fn add_stream(
        &self,
        streams: &mut Vec<SvtResolvedStream>,
        seen: &mut HashSet<String>,
        url: String,
    ) {
        if url.is_empty() || seen.contains(&url) {
            return;
        }
        seen.insert(url.clone());
        let drm = self.detect_manifest_drm(&url).await;
        streams.push(SvtResolvedStream {
            url,
            content_type: DASH_MIME_TYPE.to_string(),
            drm,
        });
    }

    async fn detect_manifest_drm(&self, manifest_url: &str) -> Option<DrmInfo> {
        let manifest = self.get_text(manifest_url).await.ok()?;
        let lowered = manifest.to_lowercase();
        let license_url = extract_license_url(&manifest)?;

        if lowered.contains(CLEARKEY_SCHEME_URI) {
            return Some(DrmInfo::new(DrmSystem::ClearKey, license_url));
        }
        if lowered.contains(WIDEVINE_SCHEME_URI) {
            return Some(DrmInfo::new(DrmSystem::Widevine, license_url));
        }
        None
    }
}

fn variant_dash_reference(video: &SvtVideoResponse, name: &str) -> Option<SvtVideoReference> {
    let variant = video.variants.get(name).and_then(Option::as_ref)?;
    pick_dash_reference(&variant.video_references).cloned()
}

fn pick_dash_reference(references: &[SvtVideoReference]) -> Option<&SvtVideoReference> {
    if let Some(reference) = references
        .iter()
        .find(|reference| reference.format.as_deref() == Some("dash-full"))
    {
        return Some(reference);
    }
    if let Some(reference) = references.iter().find(|reference| {
        reference
            .format
            .as_deref()
            .is_some_and(|f| f.starts_with("dash"))
    }) {
        return Some(reference);
    }
    references
        .iter()
        .find(|reference| reference.url.ends_with(".mpd"))
}

fn pick_dash_fallback_references(references: &[SvtVideoReference]) -> Vec<SvtVideoReference> {
    let mut fallbacks: Vec<SvtVideoReference> = Vec::new();
    for format_name in ["dash-hbbtv-avc", "dash-avc", "dash"] {
        if let Some(reference) = references
            .iter()
            .find(|reference| reference.format.as_deref() == Some(format_name))
        {
            if !fallbacks.contains(reference) {
                fallbacks.push(reference.clone());
            }
        }
    }
    fallbacks
}

fn extract_license_url(manifest: &str) -> Option<String> {
    for regex in [&*DASHIF_LAURL_RE, &*MS_LAURL_ATTR_RE, &*MS_LAURL_TEXT_RE] {
        if let Some(captures) = regex.captures(manifest) {
            let value = captures.get(1).map_or("", |m| m.as_str()).trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

fn preferred_video_track(media_custom_data: Option<&Value>) -> Option<String> {
    let object = media_custom_data?.as_object()?;
    let preference = object.get("videoTrackPreference")?.as_object()?;
    let kind = preference.get("kind")?.as_str()?;
    if kind.trim().to_lowercase() == "original" {
        Some("original".to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extracts_dashif_clearkey_license_url() {
        let manifest = concat!(
            "<MPD><Period><AdaptationSet><ContentProtection ",
            "schemeIdUri=\"urn:uuid:e2719d58-a985-b3c9-781a-b030af78d30e\">",
            "<dashif:Laurl xmlns:dashif=\"https://dashif.org/guidelines/clearKey\">",
            "https://license.example.com/clearkey</dashif:Laurl>",
            "</ContentProtection></AdaptationSet></Period></MPD>"
        );
        assert_eq!(
            extract_license_url(manifest).as_deref(),
            Some("https://license.example.com/clearkey")
        );
    }

    #[test]
    fn no_license_url_when_absent() {
        assert_eq!(extract_license_url("<MPD><Period/></MPD>"), None);
    }

    #[test]
    fn preferred_video_track_only_for_original() {
        assert_eq!(
            preferred_video_track(Some(&json!({"videoTrackPreference": {"kind": "Original"}}))),
            Some("original".to_string())
        );
        assert_eq!(
            preferred_video_track(Some(&json!({"videoTrackPreference": {"kind": "dubbed"}}))),
            None
        );
        assert_eq!(preferred_video_track(None), None);
    }

    #[test]
    fn picks_dash_full_reference_first() {
        let references = vec![
            SvtVideoReference {
                url: "https://x/a.mpd".into(),
                resolve: None,
                format: Some("dash-avc".into()),
            },
            SvtVideoReference {
                url: "https://x/b.mpd".into(),
                resolve: None,
                format: Some("dash-full".into()),
            },
        ];
        assert_eq!(
            pick_dash_reference(&references).unwrap().url,
            "https://x/b.mpd"
        );
    }
}
