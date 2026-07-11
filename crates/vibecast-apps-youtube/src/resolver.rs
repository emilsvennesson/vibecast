//! YouTube video metadata and progressive-stream resolution.

use base64::engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD};
use base64::Engine as _;
use prost::Message as _;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;
use vibecast_sdk::{MediaImage, PlaybackMedia, PlaybackStream, PlayerCapabilities, StreamType};

const CLIENT_NAME: &str = "ANDROID_VR";
const CLIENT_VERSION: &str = "1.57";
const CLIENT_USER_AGENT: &str =
    "com.google.android.apps.youtube.vr.oculus/1.57 (Linux; U; Android 12L; en_US)";

#[derive(Clone)]
pub(crate) struct Resolver {
    http: reqwest::Client,
    endpoints: Endpoints,
}

#[derive(Clone)]
struct Endpoints {
    watch: String,
    player: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            watch: "https://www.youtube.com/watch".to_string(),
            player: "https://www.youtube.com/youtubei/v1/player".to_string(),
        }
    }
}

impl Resolver {
    pub(crate) fn new(http: reqwest::Client) -> Self {
        Self {
            http,
            endpoints: Endpoints::default(),
        }
    }

    #[cfg(test)]
    fn with_endpoints(http: reqwest::Client, base: &str) -> Self {
        Self {
            http,
            endpoints: Endpoints {
                watch: format!("{base}/watch"),
                player: format!("{base}/youtubei/v1/player"),
            },
        }
    }

    pub(crate) async fn resolve(
        &self,
        video_id: &str,
        start_time: f64,
        capabilities: &PlayerCapabilities,
    ) -> Result<PlaybackMedia, ResolveError> {
        let mut watch_url = Url::parse(&self.endpoints.watch)
            .map_err(|_| ResolveError::Protocol("invalid watch endpoint"))?;
        watch_url.query_pairs_mut().append_pair("v", video_id);
        let html = self
            .http
            .get(watch_url)
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;

        let api_key = extract_config_string(&html, "INNERTUBE_API_KEY").ok_or(
            ResolveError::Protocol("watch page omitted InnerTube API key"),
        )?;
        let visitor_data = extract_config_string(&html, "VISITOR_DATA")
            .or_else(|| extract_config_string(&html, "visitorData"));

        let mut player_url = Url::parse(&self.endpoints.player)
            .map_err(|_| ResolveError::Protocol("invalid player endpoint"))?;
        player_url.query_pairs_mut().append_pair("key", &api_key);
        let response: PlayerResponse = self
            .http
            .post(player_url)
            .header("User-Agent", CLIENT_USER_AGENT)
            .json(&PlayerRequest::new(video_id, visitor_data.as_deref()))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        if response.playability_status.status != "OK" {
            return Err(ResolveError::Unplayable(
                response
                    .playability_status
                    .reason
                    .unwrap_or(response.playability_status.status),
            ));
        }

        playback_media(response, start_time, capabilities)
    }
}

#[derive(Debug, Error)]
pub(crate) enum ResolveError {
    #[error("YouTube HTTP request failed")]
    Http(#[from] reqwest::Error),
    #[error("YouTube protocol error: {0}")]
    Protocol(&'static str),
    #[error("video is unavailable: {0}")]
    Unplayable(String),
    #[error("no compatible progressive stream is available")]
    NoCompatibleStream,
}

fn extract_config_string(html: &str, key: &str) -> Option<String> {
    let marker = format!("\"{key}\":\"");
    let start = html.find(&marker)? + marker.len();
    let tail = &html[start..];
    let end = tail.find('"')?;
    Some(tail[..end].to_string())
}

#[derive(Serialize)]
struct PlayerRequest<'a> {
    #[serde(rename = "videoId")]
    video_id: &'a str,
    context: RequestContext<'a>,
}

impl<'a> PlayerRequest<'a> {
    fn new(video_id: &'a str, visitor_data: Option<&'a str>) -> Self {
        Self {
            video_id,
            context: RequestContext {
                client: ClientContext {
                    client_name: CLIENT_NAME,
                    client_version: CLIENT_VERSION,
                    visitor_data,
                    hl: "en",
                    gl: "US",
                    android_sdk_version: 32,
                },
            },
        }
    }
}

#[derive(Serialize)]
struct RequestContext<'a> {
    client: ClientContext<'a>,
}

#[derive(Serialize)]
struct ClientContext<'a> {
    #[serde(rename = "clientName")]
    client_name: &'static str,
    #[serde(rename = "clientVersion")]
    client_version: &'static str,
    #[serde(rename = "visitorData", skip_serializing_if = "Option::is_none")]
    visitor_data: Option<&'a str>,
    hl: &'static str,
    gl: &'static str,
    #[serde(rename = "androidSdkVersion")]
    android_sdk_version: u32,
}

#[derive(Debug, Deserialize)]
struct PlayerResponse {
    #[serde(rename = "playabilityStatus", default)]
    playability_status: PlayabilityStatus,
    #[serde(rename = "streamingData")]
    streaming_data: Option<StreamingData>,
    #[serde(rename = "videoDetails")]
    video_details: Option<VideoDetails>,
    captions: Option<Captions>,
}

#[derive(Debug, Default, Deserialize)]
struct PlayabilityStatus {
    #[serde(default)]
    status: String,
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct StreamingData {
    #[serde(rename = "adaptiveFormats", default)]
    adaptive_formats: Vec<AdaptiveFormat>,
}

/// One adaptive (video-only or audio-only) rendition. ANDROID_VR returns plain
/// `url`s plus byte ranges, which is exactly what a DASH `SegmentBase` manifest
/// needs.
#[derive(Debug, Deserialize)]
struct AdaptiveFormat {
    itag: Option<u32>,
    #[serde(rename = "mimeType", default)]
    mime_type: String,
    url: Option<String>,
    bitrate: Option<u64>,
    width: Option<u32>,
    height: Option<u32>,
    fps: Option<u32>,
    #[serde(rename = "initRange")]
    init_range: Option<ByteRange>,
    #[serde(rename = "indexRange")]
    index_range: Option<ByteRange>,
    #[serde(rename = "audioSampleRate")]
    audio_sample_rate: Option<String>,
    #[serde(rename = "audioChannels")]
    audio_channels: Option<u32>,
    #[serde(rename = "audioTrack")]
    audio_track: Option<AudioTrack>,
    #[serde(rename = "isDrc", default)]
    is_drc: bool,
    xtags: Option<String>,
    #[serde(rename = "colorInfo", default)]
    color_info: ColorInfo,
    #[serde(rename = "approxDurationMs")]
    approx_duration_ms: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AudioTrack {
    #[serde(rename = "audioIsDefault", default)]
    audio_is_default: bool,
    #[serde(rename = "displayName", default)]
    display_name: String,
    #[serde(default)]
    id: String,
}

#[derive(Debug, Deserialize)]
struct Captions {
    #[serde(rename = "playerCaptionsTracklistRenderer")]
    tracklist: Option<CaptionTracklist>,
}

#[derive(Debug, Deserialize)]
struct CaptionTracklist {
    #[serde(rename = "captionTracks", default)]
    caption_tracks: Vec<CaptionTrack>,
}

#[derive(Debug, Deserialize)]
struct CaptionTrack {
    #[serde(rename = "baseUrl")]
    base_url: String,
    #[serde(rename = "languageCode", default)]
    language_code: String,
    #[serde(rename = "vssId", default)]
    vss_id: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    name: Text,
}

#[derive(Debug, Default, Deserialize)]
struct Text {
    #[serde(rename = "simpleText")]
    simple_text: Option<String>,
    #[serde(default)]
    runs: Vec<TextRun>,
}

impl Text {
    fn value(&self) -> String {
        self.simple_text.clone().unwrap_or_else(|| {
            self.runs
                .iter()
                .map(|run| run.text.as_str())
                .collect::<String>()
        })
    }
}

#[derive(Debug, Deserialize)]
struct TextRun {
    #[serde(default)]
    text: String,
}

#[derive(Clone, PartialEq, prost::Message)]
struct FormatXTags {
    #[prost(message, repeated, tag = "1")]
    values: Vec<XTag>,
}

#[derive(Clone, PartialEq, prost::Message)]
struct XTag {
    #[prost(string, optional, tag = "1")]
    key: Option<String>,
    #[prost(string, optional, tag = "2")]
    value: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ByteRange {
    start: String,
    end: String,
}

#[derive(Debug, Default, Deserialize)]
struct ColorInfo {
    #[serde(default)]
    primaries: String,
    #[serde(rename = "transferCharacteristics", default)]
    transfer: String,
    #[serde(rename = "matrixCoefficients", default)]
    matrix: String,
}

#[derive(Debug, Deserialize)]
struct VideoDetails {
    #[serde(rename = "videoId")]
    video_id: Option<String>,
    title: Option<String>,
    author: Option<String>,
    #[serde(rename = "lengthSeconds")]
    length_seconds: Option<String>,
    thumbnail: Option<ThumbnailList>,
}

#[derive(Debug, Deserialize)]
struct ThumbnailList {
    #[serde(default)]
    thumbnails: Vec<Thumbnail>,
}

#[derive(Debug, Deserialize)]
struct Thumbnail {
    url: String,
    width: Option<u32>,
    height: Option<u32>,
}

fn playback_media(
    response: PlayerResponse,
    start_time: f64,
    capabilities: &PlayerCapabilities,
) -> Result<PlaybackMedia, ResolveError> {
    let PlayerResponse {
        streaming_data,
        video_details: details,
        captions,
        ..
    } = response;
    let formats = streaming_data
        .ok_or(ResolveError::NoCompatibleStream)?
        .adaptive_formats;
    let caption_tracks = captions
        .and_then(|captions| captions.tracklist)
        .map(|tracklist| tracklist.caption_tracks)
        .unwrap_or_default();

    let (manifest, stream_duration) = build_dash_manifest(&formats, &caption_tracks, capabilities)?;

    let duration = stream_duration.or_else(|| {
        details
            .as_ref()?
            .length_seconds
            .as_deref()?
            .parse::<f64>()
            .ok()
    });
    let images = details
        .as_ref()
        .and_then(|details| details.thumbnail.as_ref())
        .and_then(|list| {
            list.thumbnails
                .iter()
                .max_by_key(|thumbnail| thumbnail.width.unwrap_or_default())
        })
        .map(|thumbnail| {
            vec![MediaImage {
                url: thumbnail.url.clone(),
                width: thumbnail.width,
                height: thumbnail.height,
            }]
        })
        .unwrap_or_default();

    Ok(PlaybackMedia {
        session_id: String::new(),
        streams: vec![PlaybackStream::inline_manifest(
            manifest,
            "application/dash+xml",
        )],
        stream_type: StreamType::Buffered,
        content_id: details
            .as_ref()
            .and_then(|details| details.video_id.clone()),
        title: details.as_ref().and_then(|details| details.title.clone()),
        subtitle: details.as_ref().and_then(|details| details.author.clone()),
        images,
        duration,
        autoplay: true,
        start_time: start_time.max(0.0),
        custom_data: None,
    })
}

/// A video codec family we can package into DASH, ordered by preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VideoCodec {
    Av1,
    Vp9,
    H264,
}

impl VideoCodec {
    /// Preference order: AV1 (most efficient) > VP9 > H.264.
    const PREFERENCE: [VideoCodec; 3] = [VideoCodec::Av1, VideoCodec::Vp9, VideoCodec::H264];

    /// The neutral capability token a player advertises.
    fn token(self) -> &'static str {
        match self {
            VideoCodec::Av1 => "av1",
            VideoCodec::Vp9 => "vp9",
            VideoCodec::H264 => "h264",
        }
    }

    fn from_codecs(codecs: &str) -> Option<Self> {
        if codecs.starts_with("av01") {
            Some(VideoCodec::Av1)
        } else if codecs.starts_with("vp9") || codecs.starts_with("vp09") {
            Some(VideoCodec::Vp9)
        } else if codecs.starts_with("avc1") {
            Some(VideoCodec::H264)
        } else {
            None
        }
    }
}

/// An audio codec family we can package into DASH, ordered by preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AudioCodec {
    Opus,
    Aac,
}

impl AudioCodec {
    fn token(self) -> &'static str {
        match self {
            AudioCodec::Opus => "opus",
            AudioCodec::Aac => "aac",
        }
    }

    fn from_codecs(codecs: &str) -> Option<Self> {
        if codecs.starts_with("opus") {
            Some(AudioCodec::Opus)
        } else if codecs.starts_with("mp4a") {
            Some(AudioCodec::Aac)
        } else {
            None
        }
    }
}

/// A validated video rendition ready to package as a DASH `Representation`.
struct VideoRep<'a> {
    itag: u32,
    url: &'a str,
    codecs: &'a str,
    container: &'a str,
    family: VideoCodec,
    hdr: Option<&'static str>,
    bitrate: u64,
    width: Option<u32>,
    height: Option<u32>,
    fps: Option<u32>,
    init: &'a ByteRange,
    index: &'a ByteRange,
    color: &'a ColorInfo,
    approx_duration_ms: Option<&'a str>,
}

impl<'a> VideoRep<'a> {
    fn from_format(format: &'a AdaptiveFormat) -> Option<Self> {
        let container = container_of(&format.mime_type);
        if !container.starts_with("video/") {
            return None;
        }
        let codecs = codecs_of(&format.mime_type)?;
        Some(Self {
            itag: format.itag?,
            url: format.url.as_deref()?,
            codecs,
            container,
            family: VideoCodec::from_codecs(codecs)?,
            hdr: hdr_token(&format.color_info),
            bitrate: format.bitrate.unwrap_or_default(),
            width: format.width,
            height: format.height,
            fps: format.fps,
            init: format.init_range.as_ref()?,
            index: format.index_range.as_ref()?,
            color: &format.color_info,
            approx_duration_ms: format.approx_duration_ms.as_deref(),
        })
    }

    fn within_resolution(&self, capabilities: &PlayerCapabilities) -> bool {
        self.width.unwrap_or_default() <= capabilities.max_resolution.width
            && self.height.unwrap_or_default() <= capabilities.max_resolution.height
    }

    fn hdr_advertised(&self, capabilities: &PlayerCapabilities) -> bool {
        self.hdr
            .is_some_and(|token| capabilities.hdr_formats.iter().any(|f| f == token))
    }

    fn duration_secs(&self) -> Option<f64> {
        Some(self.approx_duration_ms?.parse::<f64>().ok()? / 1000.0)
    }
}

/// A validated audio rendition ready to package as a DASH `Representation`.
struct AudioRep<'a> {
    itag: u32,
    url: &'a str,
    codecs: &'a str,
    container: &'a str,
    codec: AudioCodec,
    bitrate: u64,
    sample_rate: Option<&'a str>,
    channels: u32,
    init: &'a ByteRange,
    index: &'a ByteRange,
    track_id: &'a str,
    language: String,
    label: String,
    kind: AudioKind,
    is_default: bool,
    is_drc: bool,
}

impl<'a> AudioRep<'a> {
    fn from_format(format: &'a AdaptiveFormat) -> Option<Self> {
        let container = container_of(&format.mime_type);
        if !container.starts_with("audio/") {
            return None;
        }
        let codecs = codecs_of(&format.mime_type)?;
        let tags = decode_xtags(format.xtags.as_deref());
        let track = format.audio_track.as_ref();
        let track_id = track
            .map(|track| track.id.as_str())
            .filter(|id| !id.is_empty())
            .unwrap_or("und");
        let language = tag_value(&tags, "lang")
            .and_then(normalize_language)
            .or_else(|| normalize_language(track_id.split('.').next().unwrap_or("")))
            .unwrap_or_else(|| "und".to_string());
        let kind = tag_value(&tags, "acont")
            .map(AudioKind::from_tag)
            .filter(|kind| *kind != AudioKind::Unspecified)
            .or_else(|| track.map(|track| AudioKind::from_label(&track.display_name)))
            .unwrap_or(AudioKind::Unspecified);
        let mut label = track
            .map(|track| track.display_name.trim())
            .unwrap_or("")
            .to_string();
        if label.is_empty() {
            label = language.clone();
        }
        if format.is_drc {
            label.push_str(" (DRC)");
        }

        Some(Self {
            itag: format.itag?,
            url: format.url.as_deref()?,
            codecs,
            container,
            codec: AudioCodec::from_codecs(codecs)?,
            bitrate: format.bitrate.unwrap_or_default(),
            sample_rate: format.audio_sample_rate.as_deref(),
            channels: format.audio_channels.unwrap_or(2),
            init: format.init_range.as_ref()?,
            index: format.index_range.as_ref()?,
            track_id,
            language,
            label,
            kind,
            is_default: track.is_some_and(|track| track.audio_is_default),
            is_drc: format.is_drc,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AudioKind {
    Original,
    Dubbed,
    AutoDubbed,
    Descriptive,
    Secondary,
    Unspecified,
}

impl AudioKind {
    fn from_tag(value: &str) -> Self {
        match value {
            "original" => Self::Original,
            "dubbed" => Self::Dubbed,
            "dubbed-auto" => Self::AutoDubbed,
            "descriptive" => Self::Descriptive,
            "secondary" => Self::Secondary,
            _ => Self::Unspecified,
        }
    }

    fn from_label(value: &str) -> Self {
        let value = value.to_ascii_lowercase();
        if value.contains("audio description") || value.contains("descriptive") {
            Self::Descriptive
        } else if value.contains("auto-dubbed") || value.contains("auto dubbed") {
            Self::AutoDubbed
        } else if value.contains("dubbed") {
            Self::Dubbed
        } else if value.contains("original") {
            Self::Original
        } else {
            Self::Unspecified
        }
    }
}

struct AudioGroup<'a> {
    track_id: &'a str,
    language: String,
    label: String,
    kind: AudioKind,
    is_default: bool,
    is_drc: bool,
    reps: Vec<&'a AudioRep<'a>>,
}

fn decode_xtags(value: Option<&str>) -> Vec<XTag> {
    let Some(value) = value else {
        return Vec::new();
    };
    let decoded_value = Url::parse(&format!("https://invalid/?xtags={value}"))
        .ok()
        .and_then(|url| {
            url.query_pairs()
                .find(|(key, _)| key == "xtags")
                .map(|(_, value)| value.into_owned())
        })
        .unwrap_or_else(|| value.to_string());
    let bytes = URL_SAFE_NO_PAD
        .decode(decoded_value.as_bytes())
        .or_else(|_| URL_SAFE.decode(decoded_value.as_bytes()));
    bytes
        .ok()
        .and_then(|bytes| FormatXTags::decode(bytes.as_slice()).ok())
        .map(|tags| tags.values)
        .unwrap_or_default()
}

fn tag_value<'a>(tags: &'a [XTag], key: &str) -> Option<&'a str> {
    tags.iter()
        .find(|tag| tag.key.as_deref() == Some(key))
        .and_then(|tag| tag.value.as_deref())
}

fn normalize_language(value: &str) -> Option<String> {
    let value = value.trim().replace('_', "-");
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return None;
    }
    Some(value)
}

fn container_of(mime: &str) -> &str {
    mime.split(';').next().unwrap_or("").trim()
}

fn codecs_of(mime: &str) -> Option<&str> {
    let start = mime.find("codecs=\"")? + "codecs=\"".len();
    let rest = &mime[start..];
    let end = rest.find('"')?;
    Some(&rest[..end])
}

/// The neutral HDR capability token for a rendition's transfer function, or
/// `None` for SDR.
fn hdr_token(color: &ColorInfo) -> Option<&'static str> {
    match color.transfer.as_str() {
        "COLOR_TRANSFER_CHARACTERISTICS_SMPTEST2084" => Some("hdr10"),
        "COLOR_TRANSFER_CHARACTERISTICS_ARIB_STD_B67" => Some("hlg"),
        _ => None,
    }
}

/// Select renditions purely from what the player advertises and package them
/// into a static on-demand DASH manifest. Returns the manifest plus the media
/// duration derived from the selected video renditions.
fn build_dash_manifest(
    formats: &[AdaptiveFormat],
    caption_tracks: &[CaptionTrack],
    capabilities: &PlayerCapabilities,
) -> Result<(String, Option<f64>), ResolveError> {
    let videos: Vec<VideoRep> = formats
        .iter()
        .filter_map(VideoRep::from_format)
        .filter(|rep| rep.within_resolution(capabilities))
        .collect();

    // Video codec family: AV1 > VP9 > H.264, gated on advertised support.
    let family = VideoCodec::PREFERENCE
        .into_iter()
        .find(|codec| {
            capabilities.supports_video_codec(codec.token())
                && videos.iter().any(|rep| rep.family == *codec)
        })
        .ok_or(ResolveError::NoCompatibleStream)?;

    // Use HDR only when the player advertises a matching format; never mix HDR
    // and SDR renditions in one AdaptationSet.
    let use_hdr = capabilities.supports_hdr()
        && videos
            .iter()
            .any(|rep| rep.family == family && rep.hdr_advertised(capabilities));
    let mut video_reps: Vec<&VideoRep> = videos
        .iter()
        .filter(|rep| {
            rep.family == family
                && if use_hdr {
                    rep.hdr_advertised(capabilities)
                } else {
                    rep.hdr.is_none()
                }
        })
        .collect();
    // A family is single-container in practice; stay defensive so one
    // AdaptationSet holds a single container.
    if let Some(&first) = video_reps.first() {
        video_reps.retain(|rep| rep.container == first.container);
    }
    video_reps.sort_by_key(|rep| rep.bitrate);
    if video_reps.is_empty() {
        return Err(ResolveError::NoCompatibleStream);
    }

    // Keep each logical YouTube audio track separate. Codec selection happens
    // per track so a language available only as AAC is not hidden by another
    // language that also offers Opus.
    let audio_all: Vec<AudioRep> = formats.iter().filter_map(AudioRep::from_format).collect();
    let mut audio_groups: Vec<AudioGroup<'_>> = Vec::new();
    for rep in &audio_all {
        let existing = audio_groups.iter_mut().find(|group| {
            group.track_id == rep.track_id && group.kind == rep.kind && group.is_drc == rep.is_drc
        });
        if let Some(group) = existing {
            group.is_default |= rep.is_default;
            group.reps.push(rep);
        } else {
            audio_groups.push(AudioGroup {
                track_id: rep.track_id,
                language: rep.language.clone(),
                label: rep.label.clone(),
                kind: rep.kind,
                is_default: rep.is_default,
                is_drc: rep.is_drc,
                reps: vec![rep],
            });
        }
    }
    for group in &mut audio_groups {
        let selected_codec = [AudioCodec::Opus, AudioCodec::Aac]
            .into_iter()
            .find(|codec| {
                capabilities.audio_codecs.iter().any(|c| c == codec.token())
                    && group.reps.iter().any(|rep| rep.codec == *codec)
            });
        match selected_codec {
            Some(codec) => group.reps.retain(|rep| rep.codec == codec),
            None => group.reps.clear(),
        }
        if let Some(container) = group.reps.first().map(|rep| rep.container) {
            group.reps.retain(|rep| rep.container == container);
        }
        group.reps.sort_by_key(|rep| rep.bitrate);
    }
    audio_groups.retain(|group| !group.reps.is_empty());
    let main_audio = audio_groups
        .iter()
        .position(|group| group.is_default && !group.is_drc)
        .or_else(|| audio_groups.iter().position(|group| group.is_default))
        .or_else(|| {
            audio_groups
                .iter()
                .position(|group| group.kind == AudioKind::Original && !group.is_drc)
        })
        .or_else(|| (!audio_groups.is_empty()).then_some(0));
    let caption_tracks = if capabilities
        .subtitle_formats
        .iter()
        .any(|format| format == "vtt")
    {
        caption_tracks
    } else {
        &[]
    };

    let duration = video_reps
        .iter()
        .filter_map(|rep| rep.duration_secs())
        .fold(None, |acc: Option<f64>, secs| {
            Some(acc.map_or(secs, |current| current.max(secs)))
        });

    Ok((
        render_mpd(
            &video_reps,
            &audio_groups,
            main_audio,
            caption_tracks,
            duration,
        ),
        duration,
    ))
}

/// CICP colour-signaling integers (ISO/IEC 23001-8) for a rendition's
/// `colorInfo`, or `None` when any component is unrecognized.
fn cicp(color: &ColorInfo) -> Option<(u8, u8, u8)> {
    let primaries = match color.primaries.as_str() {
        "COLOR_PRIMARIES_BT2020" => 9,
        "COLOR_PRIMARIES_BT709" => 1,
        _ => return None,
    };
    let transfer = match color.transfer.as_str() {
        "COLOR_TRANSFER_CHARACTERISTICS_SMPTEST2084" => 16,
        "COLOR_TRANSFER_CHARACTERISTICS_ARIB_STD_B67" => 18,
        "COLOR_TRANSFER_CHARACTERISTICS_BT709" => 1,
        _ => return None,
    };
    let matrix = match color.matrix.as_str() {
        "COLOR_MATRIX_COEFFICIENTS_BT2020_NCL" => 9,
        "COLOR_MATRIX_COEFFICIENTS_BT709" => 1,
        _ => return None,
    };
    Some((primaries, transfer, matrix))
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn xml_escape_attr(text: &str) -> String {
    xml_escape(text)
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn render_mpd(
    video_reps: &[&VideoRep],
    audio_groups: &[AudioGroup<'_>],
    main_audio: Option<usize>,
    caption_tracks: &[CaptionTrack],
    duration: Option<f64>,
) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    let _ = writeln!(
        out,
        "<MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\" \
profiles=\"urn:mpeg:dash:profile:full:2011\" type=\"static\" \
minBufferTime=\"PT1.5S\" mediaPresentationDuration=\"PT{:.3}S\">",
        duration.unwrap_or(0.0)
    );
    out.push_str("<Period>\n");

    let _ = writeln!(
        out,
        "<AdaptationSet id=\"video\" contentType=\"video\" mimeType=\"{}\" \
subsegmentAlignment=\"true\" startWithSAP=\"1\">",
        video_reps[0].container
    );
    out.push_str("<Role schemeIdUri=\"urn:mpeg:dash:role:2011\" value=\"main\"/>\n");
    if video_reps[0].hdr.is_some() {
        if let Some((primaries, transfer, matrix)) = cicp(video_reps[0].color) {
            let _ = writeln!(
                out,
                "<SupplementalProperty schemeIdUri=\"urn:mpeg:mpegB:cicp:ColourPrimaries\" value=\"{primaries}\"/>"
            );
            let _ = writeln!(
                out,
                "<SupplementalProperty schemeIdUri=\"urn:mpeg:mpegB:cicp:TransferCharacteristics\" value=\"{transfer}\"/>"
            );
            let _ = writeln!(
                out,
                "<SupplementalProperty schemeIdUri=\"urn:mpeg:mpegB:cicp:MatrixCoefficients\" value=\"{matrix}\"/>"
            );
        }
    }
    for rep in video_reps {
        let mut tag = format!(
            "<Representation id=\"{}\" codecs=\"{}\" bandwidth=\"{}\"",
            rep.itag, rep.codecs, rep.bitrate
        );
        if let Some(width) = rep.width {
            let _ = write!(tag, " width=\"{width}\"");
        }
        if let Some(height) = rep.height {
            let _ = write!(tag, " height=\"{height}\"");
        }
        if let Some(fps) = rep.fps {
            let _ = write!(tag, " frameRate=\"{fps}\"");
        }
        let _ = writeln!(out, "{tag}>");
        let _ = writeln!(out, "<BaseURL>{}</BaseURL>", xml_escape(rep.url));
        let _ = writeln!(
            out,
            "<SegmentBase indexRange=\"{}-{}\"><Initialization range=\"{}-{}\"/></SegmentBase>",
            rep.index.start, rep.index.end, rep.init.start, rep.init.end
        );
        out.push_str("</Representation>\n");
    }
    out.push_str("</AdaptationSet>\n");

    for (index, group) in audio_groups.iter().enumerate() {
        let first = group.reps[0];
        let _ = writeln!(
            out,
            "<AdaptationSet id=\"audio-{index}\" contentType=\"audio\" mimeType=\"{}\" lang=\"{}\" startWithSAP=\"1\">",
            first.container,
            xml_escape_attr(&group.language),
        );
        let _ = writeln!(out, "<Label>{}</Label>", xml_escape(&group.label));
        let role = if main_audio == Some(index) {
            "main"
        } else {
            match group.kind {
                AudioKind::Dubbed | AudioKind::AutoDubbed => "dub",
                _ => "alternate",
            }
        };
        let _ = writeln!(
            out,
            "<Role schemeIdUri=\"urn:mpeg:dash:role:2011\" value=\"{role}\"/>"
        );
        if group.kind == AudioKind::Descriptive {
            out.push_str(
                "<Accessibility schemeIdUri=\"urn:tva:metadata:cs:AudioPurposeCS:2007\" value=\"1\"/>\n",
            );
        }
        for rep in &group.reps {
            let mut tag = format!(
                "<Representation id=\"{}\" codecs=\"{}\" bandwidth=\"{}\"",
                rep.itag, rep.codecs, rep.bitrate
            );
            if let Some(rate) = rep.sample_rate {
                let _ = write!(tag, " audioSamplingRate=\"{rate}\"");
            }
            let _ = writeln!(out, "{tag}>");
            let _ = writeln!(
                out,
                "<AudioChannelConfiguration \
schemeIdUri=\"urn:mpeg:dash:23003:3:audio_channel_configuration:2011\" value=\"{}\"/>",
                rep.channels
            );
            let _ = writeln!(out, "<BaseURL>{}</BaseURL>", xml_escape(rep.url));
            let _ = writeln!(
                out,
                "<SegmentBase indexRange=\"{}-{}\"><Initialization range=\"{}-{}\"/></SegmentBase>",
                rep.index.start, rep.index.end, rep.init.start, rep.init.end
            );
            out.push_str("</Representation>\n");
        }
        out.push_str("</AdaptationSet>\n");
    }

    for (index, track) in caption_tracks.iter().enumerate() {
        let Ok(mut url) = Url::parse(&track.base_url) else {
            continue;
        };
        let query: Vec<(String, String)> = url
            .query_pairs()
            .filter(|(key, _)| key != "fmt")
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        url.query_pairs_mut()
            .clear()
            .extend_pairs(query)
            .append_pair("fmt", "vtt");
        let language = normalize_language(&track.language_code).unwrap_or_else(|| "und".into());
        let label = track.name.value();
        let role = if track.kind == "asr" {
            "caption"
        } else {
            "subtitle"
        };
        let _ = writeln!(
            out,
            "<AdaptationSet id=\"text-{index}\" contentType=\"text\" mimeType=\"text/vtt\" lang=\"{}\">",
            xml_escape_attr(&language),
        );
        let _ = writeln!(
            out,
            "<Label>{}</Label>",
            xml_escape(if label.is_empty() { &language } else { &label })
        );
        let _ = writeln!(
            out,
            "<Role schemeIdUri=\"urn:mpeg:dash:role:2011\" value=\"{role}\"/>"
        );
        let _ = writeln!(
            out,
            "<Representation id=\"text-{index}-{}\" bandwidth=\"256\" mimeType=\"text/vtt\">",
            xml_escape_attr(&track.vss_id)
        );
        let _ = writeln!(out, "<BaseURL>{}</BaseURL>", xml_escape(url.as_str()));
        out.push_str("</Representation>\n</AdaptationSet>\n");
    }

    out.push_str("</Period>\n</MPD>\n");
    out
}

pub(crate) fn extract_video_id(content_id: &str) -> Option<String> {
    let candidate = content_id.trim();
    if valid_video_id(candidate) {
        return Some(candidate.to_string());
    }

    let url = Url::parse(candidate).ok()?;
    let host = url.host_str()?.trim_start_matches("www.");
    let id = if host == "youtu.be" {
        url.path_segments()?.next().map(str::to_string)
    } else if host == "youtube.com" || host == "m.youtube.com" {
        if url.path() == "/watch" {
            url.query_pairs()
                .find_map(|(key, value)| (key == "v").then(|| value.into_owned()))
                .as_deref()
                .map(str::to_string)
        } else {
            let mut segments = url.path_segments()?;
            match segments.next() {
                Some("embed" | "shorts" | "live") => segments.next().map(str::to_string),
                _ => None,
            }
        }
    } else {
        None
    }?;

    valid_video_id(&id).then_some(id)
}

fn valid_video_id(value: &str) -> bool {
    value.len() == 11
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn extracts_supported_video_id_forms() {
        for value in [
            "dQw4w9WgXcQ",
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=42",
            "https://youtu.be/dQw4w9WgXcQ?si=test",
            "https://youtube.com/shorts/dQw4w9WgXcQ",
            "https://youtube.com/embed/dQw4w9WgXcQ",
        ] {
            assert_eq!(extract_video_id(value).as_deref(), Some("dQw4w9WgXcQ"));
        }
        assert_eq!(extract_video_id("https://example.com/dQw4w9WgXcQ"), None);
        assert_eq!(extract_video_id("too-short"), None);
    }

    use vibecast_sdk::{Resolution, StreamSource};

    /// A representative `adaptiveFormats` array: AV1/VP9/H.264 SDR ladders, a
    /// VP9 HDR (BT.2020/PQ) rendition, plus Opus and AAC audio.
    fn adaptive_formats() -> serde_json::Value {
        serde_json::json!([
            {
                "itag": 401, "mimeType": "video/mp4; codecs=\"av01.0.12M.08\"",
                "url": "https://media.example/av1-2160", "bitrate": 17000000,
                "width": 3840, "height": 2160, "fps": 25,
                "initRange": {"start": "0", "end": "700"},
                "indexRange": {"start": "701", "end": "1188"},
                "colorInfo": {"primaries": "COLOR_PRIMARIES_BT709",
                    "transferCharacteristics": "COLOR_TRANSFER_CHARACTERISTICS_BT709",
                    "matrixCoefficients": "COLOR_MATRIX_COEFFICIENTS_BT709"},
                "approxDurationMs": "213040"
            },
            {
                "itag": 313, "mimeType": "video/webm; codecs=\"vp9\"",
                "url": "https://media.example/vp9-2160", "bitrate": 18000000,
                "width": 3840, "height": 2160, "fps": 25,
                "initRange": {"start": "0", "end": "220"},
                "indexRange": {"start": "221", "end": "893"},
                "colorInfo": {"transferCharacteristics": "COLOR_TRANSFER_CHARACTERISTICS_BT709"},
                "approxDurationMs": "213040"
            },
            {
                "itag": 248, "mimeType": "video/webm; codecs=\"vp9\"",
                "url": "https://media.example/vp9-1080", "bitrate": 3000000,
                "width": 1920, "height": 1080, "fps": 25,
                "initRange": {"start": "0", "end": "219"},
                "indexRange": {"start": "220", "end": "889"},
                "approxDurationMs": "213040"
            },
            {
                "itag": 337, "mimeType": "video/webm; codecs=\"vp09.02.51.10.01.09.16.09.00\"",
                "url": "https://media.example/vp9-hdr-2160", "bitrate": 20000000,
                "width": 3840, "height": 2160, "fps": 25,
                "initRange": {"start": "0", "end": "221"},
                "indexRange": {"start": "222", "end": "900"},
                "colorInfo": {"primaries": "COLOR_PRIMARIES_BT2020",
                    "transferCharacteristics": "COLOR_TRANSFER_CHARACTERISTICS_SMPTEST2084",
                    "matrixCoefficients": "COLOR_MATRIX_COEFFICIENTS_BT2020_NCL"},
                "approxDurationMs": "213040"
            },
            {
                "itag": 137, "mimeType": "video/mp4; codecs=\"avc1.640028\"",
                "url": "https://media.example/avc-1080", "bitrate": 4000000,
                "width": 1920, "height": 1080, "fps": 25,
                "initRange": {"start": "0", "end": "741"},
                "indexRange": {"start": "742", "end": "1229"},
                "approxDurationMs": "213040"
            },
            {
                "itag": 136, "mimeType": "video/mp4; codecs=\"avc1.4d401f\"",
                "url": "https://media.example/avc-720", "bitrate": 2000000,
                "width": 1280, "height": 720, "fps": 25,
                "initRange": {"start": "0", "end": "740"},
                "indexRange": {"start": "741", "end": "1200"},
                "approxDurationMs": "213040"
            },
            {
                "itag": 251, "mimeType": "audio/webm; codecs=\"opus\"",
                "url": "https://media.example/opus", "bitrate": 130000,
                "initRange": {"start": "0", "end": "258"},
                "indexRange": {"start": "259", "end": "629"},
                "audioSampleRate": "48000", "audioChannels": 2
            },
            {
                "itag": 140, "mimeType": "audio/mp4; codecs=\"mp4a.40.2\"",
                "url": "https://media.example/aac", "bitrate": 128000,
                "initRange": {"start": "0", "end": "722"},
                "indexRange": {"start": "723", "end": "1018"},
                "audioSampleRate": "44100", "audioChannels": 2
            }
        ])
    }

    fn formats() -> Vec<AdaptiveFormat> {
        serde_json::from_value(adaptive_formats()).unwrap()
    }

    fn encoded_xtags(values: &[(&str, &str)]) -> String {
        URL_SAFE_NO_PAD.encode(
            FormatXTags {
                values: values
                    .iter()
                    .map(|(key, value)| XTag {
                        key: Some((*key).to_string()),
                        value: Some((*value).to_string()),
                    })
                    .collect(),
            }
            .encode_to_vec(),
        )
    }

    fn multilingual_formats() -> Vec<AdaptiveFormat> {
        serde_json::from_value(serde_json::json!([
            {
                "itag": 137, "mimeType": "video/mp4; codecs=\"avc1.640028\"",
                "url": "https://media.example/video-1080", "bitrate": 4000000,
                "width": 1920, "height": 1080, "fps": 30,
                "initRange": {"start": "0", "end": "700"},
                "indexRange": {"start": "701", "end": "1200"},
                "approxDurationMs": "10000"
            },
            {
                "itag": 136, "mimeType": "video/mp4; codecs=\"avc1.4d401f\"",
                "url": "https://media.example/video-720", "bitrate": 2000000,
                "width": 1280, "height": 720, "fps": 30,
                "initRange": {"start": "0", "end": "700"},
                "indexRange": {"start": "701", "end": "1200"},
                "approxDurationMs": "10000"
            },
            {
                "itag": 251, "mimeType": "audio/webm; codecs=\"opus\"",
                "url": "https://media.example/en-opus-high", "bitrate": 130000,
                "initRange": {"start": "0", "end": "258"},
                "indexRange": {"start": "259", "end": "629"},
                "audioSampleRate": "48000", "audioChannels": 2,
                "audioTrack": {"id": "en.4", "displayName": "English (Original)",
                    "audioIsDefault": true},
                "xtags": encoded_xtags(&[("lang", "en"), ("acont", "original")])
            },
            {
                "itag": 250, "mimeType": "audio/webm; codecs=\"opus\"",
                "url": "https://media.example/en-opus-low", "bitrate": 70000,
                "initRange": {"start": "0", "end": "258"},
                "indexRange": {"start": "259", "end": "629"},
                "audioSampleRate": "48000", "audioChannels": 2,
                "audioTrack": {"id": "en.4", "displayName": "English (Original)",
                    "audioIsDefault": true},
                "xtags": encoded_xtags(&[("lang", "en"), ("acont", "original")])
            },
            {
                "itag": 140, "mimeType": "audio/mp4; codecs=\"mp4a.40.2\"",
                "url": "https://media.example/en-aac", "bitrate": 128000,
                "initRange": {"start": "0", "end": "722"},
                "indexRange": {"start": "723", "end": "1018"},
                "audioSampleRate": "44100", "audioChannels": 2,
                "audioTrack": {"id": "en.4", "displayName": "English (Original)",
                    "audioIsDefault": true},
                "xtags": encoded_xtags(&[("lang", "en"), ("acont", "original")])
            },
            {
                "itag": 140, "mimeType": "audio/mp4; codecs=\"mp4a.40.2\"",
                "url": "https://media.example/es-aac", "bitrate": 128000,
                "initRange": {"start": "0", "end": "722"},
                "indexRange": {"start": "723", "end": "1018"},
                "audioSampleRate": "44100", "audioChannels": 2,
                "audioTrack": {"id": "es.3", "displayName": "Spanish (Dubbed)"},
                "xtags": encoded_xtags(&[("lang", "es"), ("acont", "dubbed")])
            },
            {
                "itag": 140, "mimeType": "audio/mp4; codecs=\"mp4a.40.2\"",
                "url": "https://media.example/en-description", "bitrate": 128000,
                "initRange": {"start": "0", "end": "722"},
                "indexRange": {"start": "723", "end": "1018"},
                "audioSampleRate": "44100", "audioChannels": 2,
                "audioTrack": {"id": "en.5", "displayName": "English description"},
                "xtags": encoded_xtags(&[("lang", "en"), ("acont", "descriptive")])
            },
            {
                "itag": 250, "mimeType": "audio/webm; codecs=\"opus\"",
                "url": "https://media.example/en-drc", "bitrate": 70000,
                "initRange": {"start": "0", "end": "258"},
                "indexRange": {"start": "259", "end": "629"},
                "audioSampleRate": "48000", "audioChannels": 2, "isDrc": true,
                "audioTrack": {"id": "en.4", "displayName": "English (Original)"},
                "xtags": encoded_xtags(&[("lang", "en"), ("acont", "original")])
            }
        ]))
        .unwrap()
    }

    fn caption_tracks() -> Vec<CaptionTrack> {
        serde_json::from_value(serde_json::json!([
            {
                "baseUrl": "https://subs.example/en?sig=a&fmt=json3",
                "languageCode": "en", "vssId": ".en",
                "name": {"simpleText": "English"}
            },
            {
                "baseUrl": "https://subs.example/sv?sig=b",
                "languageCode": "sv", "vssId": "a.sv", "kind": "asr",
                "name": {"runs": [{"text": "Swedish"}, {"text": " (auto-generated)"}]}
            }
        ]))
        .unwrap()
    }

    fn caps(video: &[&str], audio: &[&str], hdr: &[&str], max: (u32, u32)) -> PlayerCapabilities {
        PlayerCapabilities {
            video_codecs: video.iter().map(|s| s.to_string()).collect(),
            audio_codecs: audio.iter().map(|s| s.to_string()).collect(),
            hdr_formats: hdr.iter().map(|s| s.to_string()).collect(),
            max_resolution: Resolution::new(max.0, max.1),
            ..PlayerCapabilities::default()
        }
    }

    fn build(caps: &PlayerCapabilities) -> String {
        build_dash_manifest(&formats(), &[], caps).unwrap().0
    }

    #[test]
    fn prefers_av1_then_opus_when_advertised() {
        let mpd = build(&caps(
            &["av1", "vp9", "h264"],
            &["opus", "aac"],
            &[],
            (3840, 2160),
        ));
        assert!(mpd.contains("codecs=\"av01.0.12M.08\""));
        assert!(mpd.contains("mimeType=\"video/mp4\""));
        // The audio set is Opus, not AAC.
        assert!(mpd.contains("codecs=\"opus\""));
        assert!(!mpd.contains("mp4a.40.2"));
        // Other video families are excluded.
        assert!(!mpd.contains("vp9"));
        assert!(!mpd.contains("avc1"));
    }

    #[test]
    fn prefers_vp9_over_h264_when_no_av1() {
        let mpd = build(&caps(&["vp9", "h264"], &["aac"], &[], (3840, 2160)));
        assert!(mpd.contains("codecs=\"vp9\""));
        assert!(mpd.contains("mimeType=\"video/webm\""));
        assert!(mpd.contains("codecs=\"mp4a.40.2\""));
        assert!(!mpd.contains("av01"));
        assert!(!mpd.contains("avc1"));
    }

    #[test]
    fn default_capabilities_pick_h264_and_cap_resolution() {
        // Default caps: h264/hevc, aac, SDR, 1080p.
        let mpd = build(&PlayerCapabilities::default());
        assert!(mpd.contains("codecs=\"avc1.640028\"")); // 1080p avc
        assert!(mpd.contains("codecs=\"avc1.4d401f\"")); // 720p avc
        assert!(mpd.contains("codecs=\"mp4a.40.2\""));
        // No UHD/other families, no HDR.
        assert!(!mpd.contains("av01"));
        assert!(!mpd.contains("vp9"));
        assert!(!mpd.contains("SupplementalProperty"));
    }

    #[test]
    fn resolution_cap_excludes_higher_renditions() {
        let mpd = build(&caps(&["h264"], &["aac"], &[], (1280, 720)));
        assert!(mpd.contains("https://media.example/avc-720"));
        assert!(!mpd.contains("avc-1080"));
    }

    #[test]
    fn hdr_selected_only_when_advertised_with_cicp() {
        let mpd = build(&caps(&["vp9"], &["opus"], &["hdr10"], (3840, 2160)));
        // The HDR (profile-2) rendition is chosen; SDR VP9 is excluded.
        assert!(mpd.contains("vp09.02.51.10.01.09.16.09.00"));
        assert!(mpd.contains("https://media.example/vp9-hdr-2160"));
        assert!(!mpd.contains("vp9-2160"));
        // PQ transfer characteristics signaled via CICP (value 16).
        assert!(mpd.contains("urn:mpeg:mpegB:cicp:TransferCharacteristics\" value=\"16\""));
        assert!(mpd.contains("urn:mpeg:mpegB:cicp:ColourPrimaries\" value=\"9\""));
    }

    #[test]
    fn sdr_selected_when_hdr_not_advertised() {
        let mpd = build(&caps(&["vp9"], &["opus"], &[], (3840, 2160)));
        assert!(mpd.contains("https://media.example/vp9-2160"));
        assert!(!mpd.contains("vp9-hdr-2160"));
        assert!(!mpd.contains("SupplementalProperty"));
    }

    #[test]
    fn unsupported_video_codec_yields_no_compatible_stream() {
        let result = build_dash_manifest(
            &formats(),
            &[],
            &caps(&["hevc"], &["aac"], &[], (3840, 2160)),
        );
        assert!(matches!(result, Err(ResolveError::NoCompatibleStream)));
    }

    #[test]
    fn baseurl_is_xml_escaped() {
        let raw = serde_json::json!([{
            "itag": 137, "mimeType": "video/mp4; codecs=\"avc1.640028\"",
            "url": "https://media.example/v?a=1&b=2", "bitrate": 4000000,
            "width": 1920, "height": 1080, "fps": 25,
            "initRange": {"start": "0", "end": "741"},
            "indexRange": {"start": "742", "end": "1229"},
            "approxDurationMs": "1000"
        }]);
        let formats: Vec<AdaptiveFormat> = serde_json::from_value(raw).unwrap();
        let mpd = build_dash_manifest(&formats, &[], &caps(&["h264"], &["aac"], &[], (1920, 1080)))
            .unwrap()
            .0;
        assert!(mpd.contains("<BaseURL>https://media.example/v?a=1&amp;b=2</BaseURL>"));
        assert!(!mpd.contains("a=1&b=2"));
    }

    #[test]
    fn segment_base_carries_byte_ranges() {
        let mpd = build(&caps(&["vp9"], &["opus"], &[], (1920, 1080)));
        assert!(mpd.contains("<SegmentBase indexRange=\"220-889\">"));
        assert!(mpd.contains("<Initialization range=\"0-219\"/>"));
        assert!(mpd.contains("frameRate=\"25\""));
        assert!(mpd.contains("mediaPresentationDuration=\"PT213.040S\""));
    }

    #[test]
    fn renders_separate_standard_audio_tracks_and_codec_fallbacks() {
        let mpd = build_dash_manifest(
            &multilingual_formats(),
            &[],
            &caps(&["h264"], &["opus", "aac"], &[], (1920, 1080)),
        )
        .unwrap()
        .0;

        assert!(mpd.contains("<Label>English (Original)</Label>"));
        assert!(mpd.contains("lang=\"en\""));
        assert!(mpd.contains("value=\"main\""));
        assert!(mpd.contains("https://media.example/en-opus-high"));
        assert!(mpd.contains("https://media.example/en-opus-low"));
        assert!(!mpd.contains("https://media.example/en-aac"));

        assert!(mpd.contains("<Label>Spanish (Dubbed)</Label>"));
        assert!(mpd.contains("lang=\"es\""));
        assert!(mpd.contains("value=\"dub\""));
        assert!(mpd.contains("https://media.example/es-aac"));

        assert!(mpd.contains("<Label>English description</Label>"));
        assert!(mpd.contains("urn:tva:metadata:cs:AudioPurposeCS:2007"));
        assert!(mpd.contains("<Label>English (Original) (DRC)</Label>"));

        assert!(!mpd.contains(" name="));
        assert!(!mpd.contains(" default="));
        assert!(!mpd.contains(" original="));
        assert!(!mpd.contains(" impaired="));
    }

    #[test]
    fn renders_labeled_vtt_caption_adaptation_sets() {
        let mut capabilities = caps(&["h264"], &["aac"], &[], (1920, 1080));
        capabilities.subtitle_formats = vec!["vtt".into()];
        let mpd = build_dash_manifest(&multilingual_formats(), &caption_tracks(), &capabilities)
            .unwrap()
            .0;

        assert!(mpd.contains("contentType=\"text\" mimeType=\"text/vtt\" lang=\"en\""));
        assert!(mpd.contains("<Label>English</Label>"));
        assert!(mpd.contains("value=\"subtitle\""));
        assert!(mpd.contains("https://subs.example/en?sig=a&amp;fmt=vtt"));
        assert!(mpd.contains("lang=\"sv\""));
        assert!(mpd.contains("<Label>Swedish (auto-generated)</Label>"));
        assert!(mpd.contains("value=\"caption\""));
    }

    #[test]
    fn omits_captions_when_the_player_does_not_advertise_vtt() {
        let mpd = build_dash_manifest(
            &multilingual_formats(),
            &caption_tracks(),
            &caps(&["h264"], &["aac"], &[], (1920, 1080)),
        )
        .unwrap()
        .0;

        assert!(!mpd.contains("contentType=\"text\""));
    }

    #[tokio::test]
    async fn resolve_returns_inline_dash_manifest() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/watch"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"ytcfg.set({"INNERTUBE_API_KEY":"test-key","VISITOR_DATA":"visitor"});"#,
            ))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/youtubei/v1/player"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "playabilityStatus": {"status": "OK"},
                "streamingData": {"adaptiveFormats": adaptive_formats()},
                "videoDetails": {
                    "videoId": "dQw4w9WgXcQ",
                    "title": "Example",
                    "author": "Channel",
                    "thumbnail": {"thumbnails": [{
                        "url": "https://img.example/large.jpg", "width": 1280, "height": 720
                    }]}
                },
                "captions": {
                    "playerCaptionsTracklistRenderer": {
                        "captionTracks": [{
                            "baseUrl": "https://subs.example/en?sig=test",
                            "languageCode": "en", "vssId": ".en",
                            "name": {"simpleText": "English"}
                        }]
                    }
                }
            })))
            .mount(&server)
            .await;

        let resolver = Resolver::with_endpoints(reqwest::Client::new(), &server.uri());
        let mut capabilities = caps(&["av1", "vp9", "h264"], &["opus", "aac"], &[], (3840, 2160));
        capabilities.subtitle_formats = vec!["vtt".into()];
        let media = resolver
            .resolve("dQw4w9WgXcQ", 12.5, &capabilities)
            .await
            .unwrap();

        assert_eq!(media.streams.len(), 1);
        assert_eq!(media.streams[0].content_type, "application/dash+xml");
        let StreamSource::InlineManifest(manifest) = &media.streams[0].source else {
            panic!("expected an inline manifest");
        };
        assert!(manifest.starts_with("<?xml"));
        assert!(manifest.contains("<MPD"));
        assert!(manifest.contains("av01.0.12M.08"));
        assert!(manifest.contains("<Label>English</Label>"));
        assert_eq!(media.title.as_deref(), Some("Example"));
        assert_eq!(media.subtitle.as_deref(), Some("Channel"));
        assert!((media.duration.unwrap() - 213.04).abs() < 0.001);
        assert_eq!(media.start_time, 12.5);
    }
}
