//! SponsorBlock segment lookup and per-video skip state.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use reqwest::StatusCode;
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use thiserror::Error;
use tokio::sync::Mutex;
use url::Url;
use vibecast_sdk::{PlayerState, SettingDescriptor, SettingKey, SettingScope, SettingsSnapshot};

const API_ENDPOINT: &str = "https://sponsor.ajay.app/api/skipSegments";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
const PLAYBACK_DRIFT_TOLERANCE_SECONDS: f64 = 0.5;

pub(crate) const ENABLED_KEY: SettingKey<bool> = SettingKey::new("sponsorblock_enabled");
const SPONSOR_KEY: SettingKey<bool> = SettingKey::new("sponsorblock_sponsor");
const SELF_PROMO_KEY: SettingKey<bool> = SettingKey::new("sponsorblock_selfpromo");
const INTERACTION_KEY: SettingKey<bool> = SettingKey::new("sponsorblock_interaction");
const INTRO_KEY: SettingKey<bool> = SettingKey::new("sponsorblock_intro");
const OUTRO_KEY: SettingKey<bool> = SettingKey::new("sponsorblock_outro");
const PREVIEW_KEY: SettingKey<bool> = SettingKey::new("sponsorblock_preview");
const HOOK_KEY: SettingKey<bool> = SettingKey::new("sponsorblock_hook");
const MUSIC_OFFTOPIC_KEY: SettingKey<bool> = SettingKey::new("sponsorblock_music_offtopic");
const FILLER_KEY: SettingKey<bool> = SettingKey::new("sponsorblock_filler");

struct CategorySetting {
    key: SettingKey<bool>,
    category: &'static str,
    label: &'static str,
    description: &'static str,
    default: bool,
}

const CATEGORY_SETTINGS: &[CategorySetting] = &[
    CategorySetting {
        key: SPONSOR_KEY,
        category: "sponsor",
        label: "Sponsors",
        description: "Paid promotions, referrals, and direct advertisements.",
        default: true,
    },
    CategorySetting {
        key: SELF_PROMO_KEY,
        category: "selfpromo",
        label: "Unpaid self-promotion",
        description: "Promotions for the creator's own products or services.",
        default: false,
    },
    CategorySetting {
        key: INTERACTION_KEY,
        category: "interaction",
        label: "Interaction reminders",
        description: "Requests to like, subscribe, or follow.",
        default: false,
    },
    CategorySetting {
        key: INTRO_KEY,
        category: "intro",
        label: "Intros",
        description: "Intermission and introductory animations without content.",
        default: false,
    },
    CategorySetting {
        key: OUTRO_KEY,
        category: "outro",
        label: "Outros",
        description: "Endcards, credits, and concluding material without content.",
        default: false,
    },
    CategorySetting {
        key: PREVIEW_KEY,
        category: "preview",
        label: "Previews and recaps",
        description: "Previews of upcoming content and recaps of earlier content.",
        default: false,
    },
    CategorySetting {
        key: HOOK_KEY,
        category: "hook",
        label: "Hooks",
        description: "Opening clips used to hook viewers before the main content.",
        default: false,
    },
    CategorySetting {
        key: MUSIC_OFFTOPIC_KEY,
        category: "music_offtopic",
        label: "Non-music sections",
        description: "Non-music sections in music videos.",
        default: false,
    },
    CategorySetting {
        key: FILLER_KEY,
        category: "filler",
        label: "Filler",
        description: "Tangents and filler that are not essential to the content.",
        default: false,
    },
];

pub(crate) fn setting_descriptors() -> Vec<SettingDescriptor> {
    let mut settings = Vec::with_capacity(CATEGORY_SETTINGS.len() + 1);
    settings.push(SettingDescriptor::Boolean {
        key: ENABLED_KEY.as_str().to_owned(),
        label: "SponsorBlock".to_owned(),
        description: Some("Automatically skip selected community-submitted segments.".to_owned()),
        scope: SettingScope::AppPlayer,
        default: false,
    });
    settings.extend(
        CATEGORY_SETTINGS
            .iter()
            .map(|setting| SettingDescriptor::Boolean {
                key: setting.key.as_str().to_owned(),
                label: setting.label.to_owned(),
                description: Some(setting.description.to_owned()),
                scope: SettingScope::AppPlayer,
                default: setting.default,
            }),
    );
    settings
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SponsorBlockConfig {
    enabled: bool,
    categories: Vec<&'static str>,
}

impl SponsorBlockConfig {
    fn from_snapshot(snapshot: &SettingsSnapshot) -> Self {
        let enabled = bool_setting(snapshot, ENABLED_KEY, false);
        let categories = CATEGORY_SETTINGS
            .iter()
            .filter(|setting| bool_setting(snapshot, setting.key, setting.default))
            .map(|setting| setting.category)
            .collect();
        Self {
            enabled,
            categories,
        }
    }
}

fn bool_setting(snapshot: &SettingsSnapshot, key: SettingKey<bool>, default: bool) -> bool {
    snapshot.get(key).ok().flatten().unwrap_or(default)
}

#[derive(Clone)]
pub(crate) struct SponsorBlock {
    http: reqwest::Client,
    endpoint: String,
    request_timeout: Duration,
    active: Arc<Mutex<ActiveVideo>>,
}

impl SponsorBlock {
    pub(crate) fn new(http: reqwest::Client) -> Self {
        Self {
            http,
            endpoint: API_ENDPOINT.to_owned(),
            request_timeout: REQUEST_TIMEOUT,
            active: Arc::new(Mutex::new(ActiveVideo::default())),
        }
    }

    #[cfg(test)]
    fn with_endpoint(http: reqwest::Client, endpoint: String) -> Self {
        Self {
            http,
            endpoint,
            request_timeout: REQUEST_TIMEOUT,
            active: Arc::new(Mutex::new(ActiveVideo::default())),
        }
    }

    #[cfg(test)]
    fn with_endpoint_and_timeout(
        http: reqwest::Client,
        endpoint: String,
        request_timeout: Duration,
    ) -> Self {
        Self {
            http,
            endpoint,
            request_timeout,
            active: Arc::new(Mutex::new(ActiveVideo::default())),
        }
    }

    pub(crate) async fn prepare(
        &self,
        video_id: &str,
        snapshot: &SettingsSnapshot,
    ) -> PreparedSegments {
        let config = SponsorBlockConfig::from_snapshot(snapshot);
        self.prepare_with_config(video_id, &config).await
    }

    async fn prepare_with_config(
        &self,
        video_id: &str,
        config: &SponsorBlockConfig,
    ) -> PreparedSegments {
        if !config.enabled || config.categories.is_empty() {
            return PreparedSegments::empty();
        }

        match self.fetch(video_id, &config.categories).await {
            Ok(segments) => PreparedSegments { segments },
            Err(error) => {
                tracing::warn!(%error, "SponsorBlock lookup failed; continuing without skips");
                PreparedSegments::empty()
            }
        }
    }

    pub(crate) async fn activate(&self, prepared: PreparedSegments) {
        *self.active.lock().await = ActiveVideo {
            segments: prepared.segments,
            last_playback: None,
        };
    }

    #[cfg(test)]
    pub(crate) async fn activate_for_test(&self, segments: &[(f64, f64)]) {
        self.activate(PreparedSegments {
            segments: segments
                .iter()
                .map(|(start, end)| Segment {
                    start: *start,
                    end: *end,
                })
                .collect(),
        })
        .await;
    }

    pub(crate) async fn skip_target(
        &self,
        player_state: PlayerState,
        current_time: f64,
    ) -> Option<f64> {
        if !current_time.is_finite() {
            return None;
        }
        self.active
            .lock()
            .await
            .skip_target(player_state, current_time, Instant::now())
    }

    async fn fetch(
        &self,
        video_id: &str,
        categories: &[&str],
    ) -> Result<Vec<Segment>, SponsorBlockError> {
        let mut url = Url::parse(&self.endpoint)?;
        url.path_segments_mut()
            .map_err(|_| SponsorBlockError::InvalidEndpoint)?
            .push(&hash_prefix(video_id));
        {
            let mut query = url.query_pairs_mut();
            for category in categories {
                query.append_pair("category", category);
            }
            query
                .append_pair("actionType", "skip")
                .append_pair("service", "YouTube");
        }

        let response = self
            .http
            .get(url)
            .timeout(self.request_timeout)
            .send()
            .await?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(Vec::new());
        }
        let videos: Vec<ApiVideo> = response.error_for_status()?.json().await?;
        let requested_categories = categories.iter().copied().collect::<HashSet<_>>();
        let segments = videos
            .into_iter()
            .find(|video| video.video_id == video_id)
            .map(|video| normalize_segments(video.segments, &requested_categories))
            .unwrap_or_default();
        Ok(segments)
    }
}

#[derive(Debug)]
pub(crate) struct PreparedSegments {
    segments: Vec<Segment>,
}

impl PreparedSegments {
    fn empty() -> Self {
        Self {
            segments: Vec::new(),
        }
    }
}

#[derive(Debug, Default)]
struct ActiveVideo {
    segments: Vec<Segment>,
    last_playback: Option<PlaybackSample>,
}

impl ActiveVideo {
    fn skip_target(
        &mut self,
        player_state: PlayerState,
        current_time: f64,
        observed_at: Instant,
    ) -> Option<f64> {
        let previous = self.last_playback.replace(PlaybackSample {
            player_state,
            current_time,
            observed_at,
        })?;
        if player_state != PlayerState::Playing || previous.player_state != PlayerState::Playing {
            return None;
        }

        let media_elapsed = current_time - previous.current_time;
        let wall_elapsed = observed_at
            .saturating_duration_since(previous.observed_at)
            .as_secs_f64();
        if media_elapsed < 0.0 || media_elapsed > wall_elapsed + PLAYBACK_DRIFT_TOLERANCE_SECONDS {
            return None;
        }

        self.segments
            .iter()
            .find(|segment| {
                previous.current_time < segment.start
                    && current_time >= segment.start
                    && current_time < segment.end
            })
            .map(|segment| segment.end)
    }
}

#[derive(Debug, Clone, Copy)]
struct PlaybackSample {
    player_state: PlayerState,
    current_time: f64,
    observed_at: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Segment {
    start: f64,
    end: f64,
}

#[derive(Debug, Deserialize)]
struct ApiVideo {
    #[serde(rename = "videoID")]
    video_id: String,
    segments: Vec<ApiSegment>,
}

#[derive(Debug, Deserialize)]
struct ApiSegment {
    category: String,
    #[serde(rename = "actionType")]
    action_type: String,
    segment: Vec<f64>,
}

fn normalize_segments(
    segments: Vec<ApiSegment>,
    requested_categories: &HashSet<&str>,
) -> Vec<Segment> {
    let mut normalized = segments
        .into_iter()
        .filter_map(|segment| {
            if segment.action_type != "skip"
                || !requested_categories.contains(segment.category.as_str())
                || segment.segment.len() != 2
            {
                return None;
            }
            let start = segment.segment[0];
            let end = segment.segment[1];
            (start.is_finite() && end.is_finite() && start >= 0.0 && end > start)
                .then_some(Segment { start, end })
        })
        .collect::<Vec<_>>();
    normalized.sort_by(|left, right| left.start.total_cmp(&right.start));

    let mut merged: Vec<Segment> = Vec::with_capacity(normalized.len());
    for segment in normalized {
        if let Some(previous) = merged.last_mut() {
            if segment.start <= previous.end {
                previous.end = previous.end.max(segment.end);
                continue;
            }
        }
        merged.push(segment);
    }
    merged
}

fn hash_prefix(video_id: &str) -> String {
    let digest = Sha256::digest(video_id.as_bytes());
    format!("{:02x}{:02x}", digest[0], digest[1])
}

#[derive(Debug, Error)]
enum SponsorBlockError {
    #[error("invalid SponsorBlock endpoint")]
    Url(#[from] url::ParseError),
    #[error("SponsorBlock endpoint cannot accept a hash path")]
    InvalidEndpoint,
    #[error("SponsorBlock HTTP request failed")]
    Http(#[from] reqwest::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn empty_snapshot() -> Arc<SettingsSnapshot> {
        let context = vibecast_sdk::AppContext::new(
            "session",
            "transport",
            "youtube",
            reqwest::Client::new(),
            vibecast_sdk::ReceiverContext::new(
                "YouTube test",
                "Test",
                "test-device",
                std::path::PathBuf::new(),
            ),
            Arc::new(vibecast_sdk::NoopSenderChannel),
        );
        context.settings.snapshot()
    }

    #[test]
    fn hash_prefix_uses_the_first_four_sha256_characters() {
        assert_eq!(hash_prefix("dQw4w9WgXcQ"), "5f6b");
    }

    #[test]
    fn defaults_to_disabled_with_only_sponsors_selected() {
        let config = SponsorBlockConfig::from_snapshot(&empty_snapshot());

        assert!(!config.enabled);
        assert_eq!(config.categories, ["sponsor"]);
    }

    #[test]
    fn normalizes_valid_segments_and_merges_overlaps() {
        let requested = HashSet::from(["sponsor", "intro"]);
        let segments = normalize_segments(
            vec![
                ApiSegment {
                    category: "sponsor".into(),
                    action_type: "skip".into(),
                    segment: vec![20.0, 30.0],
                },
                ApiSegment {
                    category: "intro".into(),
                    action_type: "skip".into(),
                    segment: vec![10.0, 22.0],
                },
                ApiSegment {
                    category: "outro".into(),
                    action_type: "skip".into(),
                    segment: vec![40.0, 50.0],
                },
                ApiSegment {
                    category: "sponsor".into(),
                    action_type: "mute".into(),
                    segment: vec![60.0, 70.0],
                },
                ApiSegment {
                    category: "sponsor".into(),
                    action_type: "skip".into(),
                    segment: vec![f64::NAN, 80.0],
                },
                ApiSegment {
                    category: "sponsor".into(),
                    action_type: "skip".into(),
                    segment: vec![90.0],
                },
            ],
            &requested,
        );

        assert_eq!(
            segments,
            [Segment {
                start: 10.0,
                end: 30.0
            }]
        );
    }

    #[tokio::test]
    async fn fetches_hash_collision_response_and_selects_the_exact_video() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/skipSegments/5f6b"))
            .and(query_param("category", "sponsor"))
            .and(query_param("category", "intro"))
            .and(query_param("actionType", "skip"))
            .and(query_param("service", "YouTube"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "videoID": "collision01",
                    "segments": [{
                        "category": "sponsor", "actionType": "skip", "segment": [1.0, 2.0]
                    }]
                },
                {
                    "videoID": "dQw4w9WgXcQ",
                    "segments": [{
                        "category": "sponsor", "actionType": "skip", "segment": [12.0, 24.0]
                    }]
                }
            ])))
            .mount(&server)
            .await;
        let sponsorblock = SponsorBlock::with_endpoint(
            reqwest::Client::new(),
            format!("{}/api/skipSegments", server.uri()),
        );

        let segments = sponsorblock
            .fetch("dQw4w9WgXcQ", &["sponsor", "intro"])
            .await
            .unwrap();

        assert_eq!(
            segments,
            [Segment {
                start: 12.0,
                end: 24.0
            }]
        );
    }

    #[tokio::test]
    async fn not_found_is_an_empty_segment_list() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let sponsorblock = SponsorBlock::with_endpoint(
            reqwest::Client::new(),
            format!("{}/api/skipSegments", server.uri()),
        );

        assert!(sponsorblock
            .fetch("dQw4w9WgXcQ", &["sponsor"])
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn disabled_setting_does_not_make_a_request() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;
        let sponsorblock = SponsorBlock::with_endpoint(
            reqwest::Client::new(),
            format!("{}/api/skipSegments", server.uri()),
        );

        let prepared = sponsorblock.prepare("dQw4w9WgXcQ", &empty_snapshot()).await;
        sponsorblock.activate(prepared).await;

        assert_eq!(
            sponsorblock.skip_target(PlayerState::Playing, 12.0).await,
            None
        );
    }

    #[tokio::test]
    async fn malformed_error_and_timeout_responses_prepare_empty_state() {
        for response in [
            ResponseTemplate::new(200).set_body_string("not json"),
            ResponseTemplate::new(500),
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(100))
                .set_body_json(serde_json::json!([])),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .respond_with(response)
                .mount(&server)
                .await;
            let sponsorblock = SponsorBlock::with_endpoint_and_timeout(
                reqwest::Client::new(),
                format!("{}/api/skipSegments", server.uri()),
                Duration::from_millis(10),
            );
            let config = SponsorBlockConfig {
                enabled: true,
                categories: vec!["sponsor"],
            };

            let prepared = sponsorblock
                .prepare_with_config("dQw4w9WgXcQ", &config)
                .await;
            sponsorblock.activate(prepared).await;
            assert_eq!(
                sponsorblock.skip_target(PlayerState::Playing, 12.0).await,
                None
            );
        }
    }

    fn active_video() -> ActiveVideo {
        ActiveVideo {
            segments: vec![Segment {
                start: 10.0,
                end: 20.0,
            }],
            last_playback: None,
        }
    }

    #[test]
    fn crossing_again_after_rewind_skips_again() {
        let mut active = active_video();
        let started_at = Instant::now();

        assert_eq!(
            active.skip_target(PlayerState::Playing, 9.8, started_at),
            None
        );
        assert_eq!(
            active.skip_target(
                PlayerState::Playing,
                10.2,
                started_at + Duration::from_secs(1)
            ),
            Some(20.0)
        );
        assert_eq!(
            active.skip_target(
                PlayerState::Playing,
                20.0,
                started_at + Duration::from_secs(2)
            ),
            None
        );
        assert_eq!(
            active.skip_target(
                PlayerState::Playing,
                9.8,
                started_at + Duration::from_secs(3)
            ),
            None
        );
        assert_eq!(
            active.skip_target(
                PlayerState::Playing,
                10.2,
                started_at + Duration::from_secs(4)
            ),
            Some(20.0)
        );
    }

    #[test]
    fn seeking_or_starting_inside_a_segment_does_not_skip() {
        let started_at = Instant::now();
        let mut active = active_video();

        assert_eq!(
            active.skip_target(PlayerState::Playing, 9.0, started_at),
            None
        );
        assert_eq!(
            active.skip_target(
                PlayerState::Playing,
                12.0,
                started_at + Duration::from_millis(100)
            ),
            None
        );
        assert_eq!(
            active.skip_target(
                PlayerState::Playing,
                13.0,
                started_at + Duration::from_millis(1100)
            ),
            None
        );

        let mut active = active_video();
        assert_eq!(
            active.skip_target(PlayerState::Playing, 12.0, started_at),
            None
        );
        assert_eq!(
            active.skip_target(
                PlayerState::Playing,
                13.0,
                started_at + Duration::from_secs(1)
            ),
            None
        );
    }
}
