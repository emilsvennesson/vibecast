//! Bundled DAZN app.
//!
//! DAZN's Chromecast receiver (`tv.dazn.com/app/chromecast`) never receives a
//! media-namespace `LOAD`: the sender drives everything over `urn:x-cast:DAZN`
//! (`InitSession` with the user's token, then `InitPlayback` with an asset id).
//! The session resolves the asset through DAZN's Playback API and starts it via
//! the app playback controller, mirroring state back as `CurrentStateChanged` /
//! `CurrentTimeChanged`.
//!
//! Stream quality is chosen server-side from the `Capabilities` the receiver
//! reports; by default these follow the bound player's capabilities, and the
//! per-player `stream_quality` setting can force the highest ladder.

#![forbid(unsafe_code)]

mod api;
mod models;

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::Mutex;
use vibecast_sdk::{
    AppContext, AppManifest, AppProvider, AppSession, AppSettingsSchema, ChoiceOption, DrmInfo,
    DrmSecurityLevel, DrmSystem, IdleReason, LaunchCredentials, LaunchError, LicenseForwarder,
    LicenseRequest, LicenseResponse, LicenseRoute, LoadRequest, MediaResolveError,
    MessageDisposition, PlaybackMedia, PlaybackState, PlaybackStream, PlayerCapabilities,
    PlayerState, SettingDescriptor, SettingKey, SettingScope, SettingsSnapshot, StreamType,
};

use crate::api::{token_expires_within, DaznApi, DaznError, PlaybackRequest, QualityParams};
use crate::models::{
    CurrentTime, DaznErrorPayload, DaznMessage, DaznPlayerState, DaznRequest, InitPlayback,
    InitSession, PlaybackDetail, PlaybackResponse, ResumePosition,
};

const NS_DAZN: &str = "urn:x-cast:DAZN";
const APP_IDS: &[&str] = &["E1DE188D"];
const DASH_CONTENT_TYPE: &str = "application/dash+xml";
/// Refresh the access token when it expires within this many seconds (DAZN's
/// receiver refreshes five minutes ahead).
const TOKEN_REFRESH_MARGIN_SECS: u64 = 300;
/// DAZN's category for playback failures (`C011_VIDEO_PLAYBACK`).
const ERROR_CATEGORY_PLAYBACK: u32 = 11;
/// DAZN's generic playback notification code (`C10003_PLAYBACK_GENERIC`).
const ERROR_CODE_PLAYBACK_GENERIC: u32 = 10003;

const STREAM_QUALITY_KEY: SettingKey<String> = SettingKey::new("stream_quality");

/// How the Playback API `Capabilities` are chosen.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum StreamQuality {
    /// Derived from the bound player's capabilities.
    #[default]
    Auto,
    /// Always request DAZN's top ladder (UHD, HEVC, HDR, Dolby audio).
    Highest,
}

impl StreamQuality {
    fn from_snapshot(snapshot: &SettingsSnapshot) -> Self {
        snapshot
            .get(STREAM_QUALITY_KEY)
            .ok()
            .flatten()
            .as_deref()
            .map(Self::parse)
            .unwrap_or_default()
    }

    fn parse(value: &str) -> Self {
        match value {
            "highest" => Self::Highest,
            _ => Self::Auto,
        }
    }
}

/// Build the Playback API quality parameters for a player.
///
/// Mirrors DAZN's own capability probing: `hevc`/`hdr`/`dd`/`ddp` follow codec
/// and HDR support, `4k` additionally requires a UHD output and hardware
/// (L1) Widevine, and `drmSecLvl=low` is only sent for software Widevine.
fn quality_params(caps: &PlayerCapabilities, quality: StreamQuality) -> QualityParams {
    if quality == StreamQuality::Highest {
        return QualityParams {
            capabilities: "4k,dd,ddp,hdr,hevc,mta".to_string(),
            drm_security_level: None,
        };
    }

    let widevine = caps.drm_level(DrmSystem::Widevine);
    let hardware_drm = matches!(widevine, Some(DrmSecurityLevel::L1));
    let hevc = caps.supports_video_codec("hevc");
    let has_audio = |codec: &str| caps.audio_codecs.iter().any(|c| c == codec);

    let mut flags = vec!["mta"];
    if hevc {
        flags.push("hevc");
    }
    if hevc && hardware_drm && caps.max_resolution.height >= 2160 {
        flags.push("4k");
    }
    if caps
        .hdr_formats
        .iter()
        .any(|format| format == "hdr10" || format == "hlg")
    {
        flags.push("hdr");
    }
    if has_audio("ac3") {
        flags.push("dd");
    }
    if has_audio("eac3") {
        flags.push("ddp");
    }
    flags.sort_unstable();

    QualityParams {
        capabilities: flags.join(","),
        drm_security_level: matches!(widevine, Some(DrmSecurityLevel::L3)).then_some("low"),
    }
}

/// DAZN app provider.
#[derive(Debug, Default)]
pub struct Dazn;

impl Dazn {
    /// Construct the provider.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl AppProvider for Dazn {
    fn manifest(&self) -> AppManifest {
        let settings = AppSettingsSchema::with_display_name(
            "dazn",
            "DAZN",
            vec![SettingDescriptor::Choice {
                key: STREAM_QUALITY_KEY.as_str().to_owned(),
                label: "Stream quality".to_owned(),
                description: Some(
                    "Automatic requests what this player reports it supports. Highest always \
                     requests DAZN's top quality (UHD, HEVC, HDR, Dolby audio)."
                        .to_owned(),
                ),
                scope: SettingScope::AppPlayer,
                default: "auto".to_owned(),
                choices: vec![
                    ChoiceOption::new("auto", "Automatic"),
                    ChoiceOption::new("highest", "Highest available"),
                ],
            }],
        )
        .expect("static DAZN settings must be valid");
        AppManifest::new("dazn", APP_IDS, "DAZN", settings).with_namespaces(&[NS_DAZN])
    }

    async fn launch(
        &self,
        ctx: &AppContext,
        _credentials: LaunchCredentials,
    ) -> Result<Arc<dyn AppSession>, LaunchError> {
        Ok(Arc::new(DaznSession {
            api: DaznApi::new(ctx.http.clone()),
            state: Mutex::new(DaznState::default()),
        }))
    }
}

#[derive(Default)]
struct DaznState {
    token: Option<String>,
    device_guid: Option<String>,
    language: Option<String>,
    asset_id: Option<String>,
    /// Legacy MPX release PID keyed by license URL; empty for proxy-mode assets.
    mpx_release_pid: Option<(String, String)>,
}

/// A running DAZN session.
struct DaznSession {
    api: DaznApi,
    state: Mutex<DaznState>,
}

#[async_trait]
impl AppSession for DaznSession {
    async fn resolve_media(
        &self,
        _ctx: &AppContext,
        _request: &LoadRequest,
    ) -> Result<PlaybackMedia, MediaResolveError> {
        // The DAZN sender starts playback with `InitPlayback` on its own
        // namespace, never a media LOAD.
        Err(MediaResolveError::invalid_request("USE_DAZN_NAMESPACE"))
    }

    async fn on_message(
        &self,
        ctx: &AppContext,
        namespace: &str,
        data: &Value,
    ) -> MessageDisposition {
        if namespace != NS_DAZN {
            return MessageDisposition::Unhandled;
        }
        let request = match serde_json::from_value::<DaznRequest>(data.clone()) {
            Ok(request) => request,
            Err(error) => {
                let kind = data.get("type").and_then(Value::as_str).unwrap_or("?");
                tracing::debug!(%error, kind, "ignoring unrecognised DAZN message");
                return MessageDisposition::Unhandled;
            }
        };

        let playback = ctx.playback_controller();
        match request {
            DaznRequest::InitSession(init) => self.handle_init_session(ctx, init).await,
            DaznRequest::InitPlayback(init) => self.handle_init_playback(ctx, init).await,
            DaznRequest::Play => playback.play().await,
            DaznRequest::Pause => playback.pause().await,
            DaznRequest::Seek(position) => playback.seek(position.max(0.0)).await,
            DaznRequest::KillPlayback => {
                self.state.lock().await.asset_id = None;
                playback.stop().await;
            }
            DaznRequest::Disconnect => {
                self.state.lock().await.asset_id = None;
                playback.stop().await;
                ctx.broadcast_custom(NS_DAZN, DaznMessage::Disconnect(None))
                    .await;
            }
            DaznRequest::SetAudioTrack(_)
            | DaznRequest::SetCaptionTrack(_)
            | DaznRequest::DiagnosticsVisibilityToggle(_) => {}
        }
        MessageDisposition::Handled
    }

    async fn resolve_license(
        &self,
        _ctx: &AppContext,
        request: LicenseRequest,
        route: LicenseRoute,
        forward: &dyn LicenseForwarder,
    ) -> LicenseResponse {
        let (release_pid, token) = {
            let state = self.state.lock().await;
            let release_pid = state
                .mpx_release_pid
                .as_ref()
                .filter(|(url, _)| *url == route.upstream_url)
                .map(|(_, pid)| pid.clone());
            (release_pid, state.token.clone())
        };
        // Proxy-mode licenses take the raw challenge unchanged.
        let Some(release_pid) = release_pid else {
            return forward.forward(request, route).await;
        };
        let Some(token) = token else {
            return error_license(403, "not authenticated");
        };
        match self
            .api
            .mpx_widevine_license(&route.upstream_url, &token, &release_pid, &request.body)
            .await
        {
            Ok(license) => LicenseResponse::ok(license),
            Err(error) => {
                tracing::warn!(status = ?error.status(), "DAZN MPX license request failed");
                error_license(error.status().unwrap_or(502), "license request failed")
            }
        }
    }

    async fn on_playback_update(&self, ctx: &AppContext, state: PlaybackState) {
        let player_state = dazn_state(&state);
        let has_asset = {
            let mut guard = self.state.lock().await;
            if state.idle_reason.is_some() {
                // Playback ended or was stopped; allow the same asset again.
                guard.asset_id = None;
            }
            guard.asset_id.is_some()
        };
        ctx.broadcast_custom(NS_DAZN, DaznMessage::CurrentStateChanged(player_state))
            .await;
        let time = if has_asset {
            CurrentTime {
                current_display_time: state.current_time.max(0.0).floor() as i64,
                end_display_time: state.duration.unwrap_or(0.0).max(0.0).round() as i64,
                time_offset: 0.0,
            }
        } else {
            CurrentTime {
                current_display_time: 0,
                end_display_time: 0,
                time_offset: 0.0,
            }
        };
        ctx.broadcast_custom(NS_DAZN, DaznMessage::CurrentTimeChanged(time))
            .await;
    }

    async fn on_stop(&self, _ctx: &AppContext) {
        let mut state = self.state.lock().await;
        state.asset_id = None;
        state.mpx_release_pid = None;
    }
}

impl DaznSession {
    async fn handle_init_session(&self, ctx: &AppContext, init: InitSession) {
        {
            let mut state = self.state.lock().await;
            if let Some(token) = init.token() {
                state.token = Some(token.to_string());
            }
            if let Some(guid) = init.device_guid() {
                state.device_guid = Some(guid.to_string());
            }
            if let Some(language) = init.language.clone().filter(|l| !l.is_empty()) {
                state.language = Some(language);
            }
        }
        tracing::info!("DAZN session initialised");
        ctx.broadcast_custom(NS_DAZN, DaznMessage::SessionInitialized(None))
            .await;
        ctx.broadcast_custom(
            NS_DAZN,
            DaznMessage::CurrentStateChanged(DaznPlayerState::Void),
        )
        .await;
    }

    async fn handle_init_playback(&self, ctx: &AppContext, init: InitPlayback) {
        if self.state.lock().await.asset_id.as_deref() == Some(init.asset_id.as_str()) {
            // DAZN's receiver ignores repeated InitPlayback for the current asset.
            return;
        }
        match self.start_playback(ctx, &init).await {
            Ok(media) => {
                // The CDN package directory (e.g. `hevc-hdr-tv`, `tv25f`) names
                // the quality ladder DAZN chose; it carries no secrets.
                let package = media
                    .streams
                    .first()
                    .and_then(|stream| stream.source.as_url())
                    .and_then(|url| url.split('?').next())
                    .and_then(|path| path.rsplit('/').nth(1))
                    .unwrap_or_default()
                    .to_string();
                tracing::info!(stream_type = ?media.stream_type, %package, "DAZN playback resolved");
                ctx.playback_controller().load(media).await;
            }
            Err(error) => {
                tracing::warn!(status = ?error.status(), %error, "DAZN playback failed");
                self.state.lock().await.asset_id = None;
                ctx.broadcast_custom(NS_DAZN, error_message(&error)).await;
                ctx.broadcast_custom(
                    NS_DAZN,
                    DaznMessage::CurrentStateChanged(DaznPlayerState::Error),
                )
                .await;
            }
        }
    }

    async fn start_playback(
        &self,
        ctx: &AppContext,
        init: &InitPlayback,
    ) -> Result<PlaybackMedia, DaznError> {
        let (mut token, device_guid, language) = {
            let state = self.state.lock().await;
            let token = state.token.clone().ok_or(DaznError::Status(401))?;
            let guid = state
                .device_guid
                .clone()
                .unwrap_or_else(|| ctx.receiver.device_id.clone());
            let language = state.language.clone().unwrap_or_else(|| "en".to_string());
            (token, guid, language)
        };

        if token_expires_within(&token, TOKEN_REFRESH_MARGIN_SECS) {
            token = self.refresh(&token, &device_guid).await?;
        }

        let quality = quality_params(
            &ctx.receiver.capabilities,
            StreamQuality::from_snapshot(&ctx.settings.snapshot()),
        );
        tracing::info!(
            capabilities = %quality.capabilities,
            drm_sec_lvl = quality.drm_security_level.unwrap_or("-"),
            "requesting DAZN playback"
        );
        let audio_language = init
            .audio_track
            .as_ref()
            .and_then(|track| track.language.as_deref())
            .filter(|language| !language.is_empty());
        let request = PlaybackRequest {
            token: &token,
            device_guid: &device_guid,
            asset_id: &init.asset_id,
            language: &language,
            audio_language,
            youth_protection_pin: init.youth_protection_pin.as_deref(),
            quality: &quality,
        };
        let response = match self.api.playback(&request).await {
            Err(error) if error.is_auth() => {
                // DAZN retries once after refreshing on 401/403.
                let fresh = self.refresh(&token, &device_guid).await?;
                self.api
                    .playback(&PlaybackRequest {
                        token: &fresh,
                        ..request
                    })
                    .await?
            }
            other => other?,
        };

        let (media, mpx) = build_media(&ctx.session_id, init, response)?;
        let mut state = self.state.lock().await;
        state.asset_id = Some(init.asset_id.clone());
        state.mpx_release_pid = mpx;
        Ok(media)
    }

    async fn refresh(&self, token: &str, device_guid: &str) -> Result<String, DaznError> {
        let fresh = self.api.refresh_token(token, device_guid).await?;
        self.state.lock().await.token = Some(fresh.clone());
        tracing::debug!("DAZN access token refreshed");
        Ok(fresh)
    }
}

/// Pick the preferred CDN and build the media, returning the MPX
/// `(license_url, release_pid)` pair when the asset uses the legacy license
/// flow.
fn build_media(
    session_id: &str,
    init: &InitPlayback,
    response: PlaybackResponse,
) -> Result<(PlaybackMedia, Option<(String, String)>), DaznError> {
    let detail = preferred_detail(&response).ok_or(DaznError::MissingField("PlaybackDetails"))?;
    let manifest_url = detail
        .manifest_url
        .as_deref()
        .filter(|url| !url.is_empty())
        .ok_or(DaznError::MissingField("ManifestUrl"))?;
    // DAZN's player appends the CDN token to the manifest and every segment
    // request; the CDN rejects untokenised segments with 401.
    let mut stream = match cdn_token_query(detail) {
        Some(query) => PlaybackStream::url(with_query(manifest_url, &query), DASH_CONTENT_TYPE)
            .with_segment_query(query),
        None => PlaybackStream::url(manifest_url, DASH_CONTENT_TYPE),
    };
    let mut mpx = None;
    if let Some(license_url) = detail.la_url.as_deref().filter(|url| !url.is_empty()) {
        stream = stream.with_drm(DrmInfo::new(DrmSystem::Widevine, license_url));
        let legacy = response
            .license
            .as_ref()
            .and_then(|license| license.mode.as_deref())
            == Some("mpx");
        if legacy {
            let pid = detail.release_pid.clone().unwrap_or_default();
            mpx = Some((license_url.to_string(), pid));
        }
    }

    let live = response.asset.is_live || response.asset.is_linear;
    let mut media = PlaybackMedia::new(
        session_id,
        vec![stream],
        if live {
            StreamType::Live
        } else {
            StreamType::Buffered
        },
    );
    media.content_id = Some(init.asset_id.clone());
    media.title = response.asset.title.clone();
    media.start_time = match &init.resume_position {
        Some(ResumePosition::Seconds(seconds)) if !live && *seconds >= 0.5 => *seconds,
        _ => 0.0,
    };
    Ok((media, mpx))
}

/// The first playback detail in DAZN's CDN precedence order.
fn preferred_detail(response: &PlaybackResponse) -> Option<&PlaybackDetail> {
    let usable = |detail: &&PlaybackDetail| {
        detail
            .manifest_url
            .as_deref()
            .is_some_and(|url| !url.is_empty())
    };
    response
        .playback_precision
        .iter()
        .flat_map(|precision| precision.cdns.iter())
        .find_map(|cdn| {
            response
                .playback_details
                .iter()
                .filter(usable)
                .find(|detail| detail.cdn_name.as_deref() == Some(cdn.as_str()))
        })
        .or_else(|| response.playback_details.iter().find(usable))
}

/// The CDN token as an encoded `name=value` query pair.
fn cdn_token_query(detail: &PlaybackDetail) -> Option<String> {
    let token = detail.cdn_token.as_ref()?;
    let name = token.name.as_deref().filter(|name| !name.is_empty())?;
    let value = token.value.as_deref().filter(|value| !value.is_empty())?;
    let mut encoded = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    Some(format!("{name}={encoded}"))
}

fn with_query(url: &str, query: &str) -> String {
    let separator = if url.contains('?') { '&' } else { '?' };
    format!("{url}{separator}{query}")
}

fn dazn_state(state: &PlaybackState) -> DaznPlayerState {
    match (state.player_state, state.idle_reason) {
        (PlayerState::Playing, _) => DaznPlayerState::Playing,
        (PlayerState::Paused, _) => DaznPlayerState::Paused,
        (PlayerState::Buffering, _) => DaznPlayerState::Buffering,
        (PlayerState::Idle, Some(IdleReason::Finished)) => DaznPlayerState::Ended,
        (PlayerState::Idle, Some(IdleReason::Error)) => DaznPlayerState::Error,
        (PlayerState::Idle, _) => DaznPlayerState::Void,
    }
}

fn error_message(error: &DaznError) -> DaznMessage {
    let status = error.status().unwrap_or(0);
    DaznMessage::ErrorOccurred(DaznErrorPayload {
        app_error_category: ERROR_CATEGORY_PLAYBACK,
        app_error_code: ERROR_CODE_PLAYBACK_GENERIC,
        category: ERROR_CATEGORY_PLAYBACK,
        status,
    })
}

fn error_license(status: u16, message: &str) -> LicenseResponse {
    LicenseResponse {
        body: message.as_bytes().to_vec(),
        content_type: "application/octet-stream".to_string(),
        status,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex as StdMutex};

    use serde_json::json;
    use vibecast_sdk::{
        DrmCapability, HeaderMap, PlaybackController, ReceiverContext, Resolution, SenderChannel,
        StreamSource,
    };
    use wiremock::matchers::{header, method, path, query_param, query_param_is_missing};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::api::{test_jwt, DaznApiConfig};

    use super::*;

    #[derive(Default, Clone)]
    struct RecordingSender {
        messages: Arc<StdMutex<Vec<(String, Value)>>>,
    }

    #[async_trait]
    impl SenderChannel for RecordingSender {
        async fn send_custom(&self, namespace: &str, data: Value) {
            self.messages
                .lock()
                .unwrap()
                .push((namespace.to_string(), data));
        }
        async fn broadcast_custom(&self, namespace: &str, data: Value) {
            self.messages
                .lock()
                .unwrap()
                .push((namespace.to_string(), data));
        }
    }

    #[derive(Debug, PartialEq)]
    enum Command {
        Load(Box<PlaybackMedia>),
        Play,
        Pause,
        Seek(f64),
        Stop,
    }

    #[derive(Default, Clone)]
    struct RecordingPlayback {
        commands: Arc<StdMutex<Vec<Command>>>,
    }

    #[async_trait]
    impl PlaybackController for RecordingPlayback {
        async fn load(&self, media: PlaybackMedia) {
            self.commands
                .lock()
                .unwrap()
                .push(Command::Load(Box::new(media)));
        }
        async fn play(&self) {
            self.commands.lock().unwrap().push(Command::Play);
        }
        async fn pause(&self) {
            self.commands.lock().unwrap().push(Command::Pause);
        }
        async fn seek(&self, position: f64) {
            self.commands.lock().unwrap().push(Command::Seek(position));
        }
        async fn stop(&self) {
            self.commands.lock().unwrap().push(Command::Stop);
        }
    }

    struct Harness {
        ctx: AppContext,
        sender: RecordingSender,
        playback: RecordingPlayback,
    }

    impl Harness {
        fn new(capabilities: PlayerCapabilities) -> Self {
            let sender = RecordingSender::default();
            let playback = RecordingPlayback::default();
            let mut receiver = ReceiverContext::new(
                "Living Room",
                "Chromecast",
                "receiver-device-id",
                PathBuf::from("/tmp/vibecast-tests/apps/dazn"),
            );
            receiver.capabilities = capabilities;
            let ctx = AppContext::new(
                "sess-1",
                "pid-1",
                APP_IDS[0],
                reqwest::Client::new(),
                receiver,
                Arc::new(sender.clone()),
            )
            .with_playback_controller(Arc::new(playback.clone()));
            Self {
                ctx,
                sender,
                playback,
            }
        }

        fn messages(&self) -> Vec<Value> {
            self.sender
                .messages
                .lock()
                .unwrap()
                .iter()
                .map(|(_, data)| data.clone())
                .collect()
        }

        fn loaded(&self) -> Vec<PlaybackMedia> {
            self.playback
                .commands
                .lock()
                .unwrap()
                .iter()
                .filter_map(|command| match command {
                    Command::Load(media) => Some((**media).clone()),
                    _ => None,
                })
                .collect()
        }
    }

    fn session(server: &MockServer) -> DaznSession {
        let config = DaznApiConfig {
            playback_url: format!("{}/v5/Playback", server.uri()),
            refresh_url: format!("{}/v5/RefreshAccessToken", server.uri()),
        };
        DaznSession {
            api: DaznApi::with_config(reqwest::Client::new(), config),
            state: Mutex::new(DaznState::default()),
        }
    }

    fn fresh_token(marker: &str) -> String {
        test_jwt(&json!({ "exp": 4_102_444_800_u64, "marker": marker, "mpx": "mpx-token" }))
    }

    fn uhd_player() -> PlayerCapabilities {
        PlayerCapabilities {
            drm: vec![DrmCapability::new(
                DrmSystem::Widevine,
                Some(DrmSecurityLevel::L1),
            )],
            video_codecs: vec!["h264".into(), "hevc".into()],
            audio_codecs: vec!["aac".into(), "ac3".into(), "eac3".into()],
            max_resolution: Resolution::new(3840, 2160),
            hdr_formats: vec!["hdr10".into(), "hlg".into()],
            ..PlayerCapabilities::default()
        }
    }

    fn browser_player() -> PlayerCapabilities {
        PlayerCapabilities {
            drm: vec![DrmCapability::new(
                DrmSystem::Widevine,
                Some(DrmSecurityLevel::L3),
            )],
            video_codecs: vec!["h264".into()],
            audio_codecs: vec!["aac".into(), "opus".into()],
            max_resolution: Resolution::hd_1080(),
            ..PlayerCapabilities::default()
        }
    }

    fn init_session(token: &str) -> Value {
        json!({
            "type": "InitSession",
            "data": {
                "token": { "source": token },
                "device": { "guid": "device-guid-1", "type": "phone" },
                "platform": "android",
                "language": "de",
                "devMode": { "isDevModeEnabled": false }
            }
        })
    }

    fn init_playback(asset_id: &str, resume: Value) -> Value {
        json!({
            "type": "InitPlayback",
            "data": { "assetId": asset_id, "eventId": "evt", "resumePosition": resume }
        })
    }

    fn playback_body(live: bool, license_mode: &str) -> Value {
        json!({
            "Asset": { "Id": "asset-1", "Title": "Match", "IsLive": live, "IsLinear": false },
            "PlaybackPrecision": { "Cdns": ["cdn-b", "cdn-a"] },
            "PlaybackDetails": [
                {
                    "CdnName": "cdn-a",
                    "ManifestUrl": "https://a.example/stream.mpd",
                    "LaUrl": "https://lic.example/a"
                },
                {
                    "CdnName": "cdn-b",
                    "ManifestUrl": "https://b.example/stream.mpd?x=1",
                    "LaUrl": "https://lic.example/wv?releasePid=rp",
                    "ReleasePid": "rp",
                    "CdnToken": { "Name": "hdntl", "Value": "exp=1~acl=/*" }
                }
            ],
            "License": { "Mode": license_mode }
        })
    }

    #[test]
    fn manifest_declares_namespace_and_quality_setting() {
        let manifest = Dazn::new().manifest();
        assert_eq!(manifest.app_key, "dazn");
        assert_eq!(manifest.app_ids, &["E1DE188D"]);
        assert_eq!(manifest.namespaces, &[NS_DAZN]);
        assert!(manifest
            .settings
            .settings()
            .iter()
            .any(|setting| setting.key() == "stream_quality"));
    }

    #[test]
    fn quality_follows_uhd_player_capabilities() {
        let params = quality_params(&uhd_player(), StreamQuality::Auto);
        assert_eq!(params.capabilities, "4k,dd,ddp,hdr,hevc,mta");
        assert_eq!(params.drm_security_level, None);
    }

    #[test]
    fn quality_is_limited_for_software_drm_players() {
        let params = quality_params(&browser_player(), StreamQuality::Auto);
        assert_eq!(params.capabilities, "mta");
        assert_eq!(params.drm_security_level, Some("low"));
    }

    #[test]
    fn uhd_requires_hardware_drm() {
        let mut caps = uhd_player();
        caps.drm = vec![DrmCapability::new(
            DrmSystem::Widevine,
            Some(DrmSecurityLevel::L3),
        )];
        let params = quality_params(&caps, StreamQuality::Auto);
        assert_eq!(params.capabilities, "dd,ddp,hdr,hevc,mta");
        assert_eq!(params.drm_security_level, Some("low"));
    }

    #[test]
    fn highest_setting_overrides_player_capabilities() {
        let params = quality_params(&browser_player(), StreamQuality::Highest);
        assert_eq!(params.capabilities, "4k,dd,ddp,hdr,hevc,mta");
        assert_eq!(params.drm_security_level, None);
        assert_eq!(StreamQuality::parse("highest"), StreamQuality::Highest);
        assert_eq!(StreamQuality::parse("auto"), StreamQuality::Auto);
        assert_eq!(StreamQuality::parse("bogus"), StreamQuality::Auto);
    }

    #[test]
    fn parses_sender_messages() {
        let init: DaznRequest = serde_json::from_value(init_session("tok")).unwrap();
        let DaznRequest::InitSession(init) = init else {
            panic!("expected InitSession");
        };
        assert_eq!(init.token(), Some("tok"));
        assert_eq!(init.device_guid(), Some("device-guid-1"));

        let playback: DaznRequest =
            serde_json::from_value(init_playback("a1", json!("JOIN_LIVE"))).unwrap();
        let DaznRequest::InitPlayback(playback) = playback else {
            panic!("expected InitPlayback");
        };
        assert_eq!(
            playback.resume_position,
            Some(ResumePosition::Named("JOIN_LIVE".into()))
        );

        let seek: DaznRequest =
            serde_json::from_value(json!({"type": "Seek", "data": 42.5})).unwrap();
        assert!(matches!(seek, DaznRequest::Seek(position) if position == 42.5));
        for kind in ["Play", "Pause", "KillPlayback", "Disconnect"] {
            let message = json!({ "type": kind, "data": null });
            assert!(
                serde_json::from_value::<DaznRequest>(message).is_ok(),
                "{kind}"
            );
        }
        let track = json!({"type": "SetAudioTrack", "data": {"id": "1", "language": "en"}});
        assert!(serde_json::from_value::<DaznRequest>(track).is_ok());
    }

    #[test]
    fn serialises_receiver_messages() {
        assert_eq!(
            serde_json::to_value(DaznMessage::CurrentStateChanged(DaznPlayerState::Playing))
                .unwrap(),
            json!({"type": "CurrentStateChanged", "data": "PLAYING"})
        );
        assert_eq!(
            serde_json::to_value(DaznMessage::SessionInitialized(None)).unwrap(),
            json!({"type": "SessionInitialized", "data": null})
        );
        assert_eq!(
            serde_json::to_value(DaznMessage::CurrentTimeChanged(CurrentTime {
                current_display_time: 12,
                end_display_time: 90,
                time_offset: 0.0,
            }))
            .unwrap(),
            json!({
                "type": "CurrentTimeChanged",
                "data": {"currentDisplayTime": 12, "endDisplayTime": 90, "timeOffset": 0.0}
            })
        );
        let error = serde_json::to_value(error_message(&DaznError::Status(403))).unwrap();
        assert_eq!(error["type"], "ErrorOccured");
        assert_eq!(error["data"]["status"], 403);
    }

    #[tokio::test]
    async fn init_session_then_playback_loads_preferred_cdn_with_widevine() {
        let server = MockServer::start().await;
        let token = fresh_token("a");
        Mock::given(method("GET"))
            .and(path("/v5/Playback"))
            .and(header("authorization", format!("Bearer {token}").as_str()))
            .and(header("x-dazn-device", "device-guid-1"))
            .and(query_param("AssetId", "asset-1"))
            .and(query_param("Capabilities", "4k,dd,ddp,hdr,hevc,mta"))
            .and(query_param("DrmType", "WIDEVINE"))
            .and(query_param("Model", "Google TV Streamer"))
            .and(query_param("Platform", "chromecast"))
            // An Android user agent gets DAZN's 720p mobile ladder.
            .and(wiremock::matchers::header_regex(
                "user-agent",
                "^[^(]*\\(X11; Linux[^)]*\\).*CrKey/",
            ))
            .and(query_param("Format", "MPEG-DASH"))
            .and(query_param("LanguageCode", "de"))
            .and(query_param_is_missing("drmSecLvl"))
            .respond_with(ResponseTemplate::new(200).set_body_json(playback_body(false, "proxy")))
            .expect(1)
            .mount(&server)
            .await;

        let session = session(&server);
        let harness = Harness::new(uhd_player());
        assert_eq!(
            session
                .on_message(&harness.ctx, NS_DAZN, &init_session(&token))
                .await,
            MessageDisposition::Handled
        );
        session
            .on_message(
                &harness.ctx,
                NS_DAZN,
                &init_playback("asset-1", json!(120.0)),
            )
            .await;

        let messages = harness.messages();
        assert_eq!(messages[0]["type"], "SessionInitialized");
        assert_eq!(
            messages[1],
            json!({"type": "CurrentStateChanged", "data": "VOID"})
        );

        let loaded = harness.loaded();
        assert_eq!(loaded.len(), 1);
        let media = &loaded[0];
        assert_eq!(media.stream_type, StreamType::Buffered);
        assert_eq!(media.start_time, 120.0);
        assert_eq!(media.title.as_deref(), Some("Match"));
        let stream = &media.streams[0];
        assert_eq!(
            stream.source,
            StreamSource::Url("https://b.example/stream.mpd?x=1&hdntl=exp%3D1~acl%3D%2F%2A".into())
        );
        assert_eq!(stream.content_type, DASH_CONTENT_TYPE);
        assert_eq!(
            stream.segment_query.as_deref(),
            Some("hdntl=exp%3D1~acl%3D%2F%2A")
        );
        let drm = stream.drm.as_ref().unwrap();
        assert_eq!(drm.system, DrmSystem::Widevine);
        assert_eq!(drm.license_url, "https://lic.example/wv?releasePid=rp");

        // A repeated InitPlayback for the same asset is ignored.
        session
            .on_message(&harness.ctx, NS_DAZN, &init_playback("asset-1", json!(0)))
            .await;
        assert_eq!(harness.loaded().len(), 1);
    }

    #[tokio::test]
    async fn live_assets_start_at_the_live_edge() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v5/Playback"))
            .and(query_param("drmSecLvl", "low"))
            .and(query_param("Capabilities", "mta"))
            .respond_with(ResponseTemplate::new(200).set_body_json(playback_body(true, "proxy")))
            .mount(&server)
            .await;

        let session = session(&server);
        let harness = Harness::new(browser_player());
        session
            .on_message(&harness.ctx, NS_DAZN, &init_session(&fresh_token("a")))
            .await;
        session
            .on_message(&harness.ctx, NS_DAZN, &init_playback("live-1", json!(3000)))
            .await;

        let media = &harness.loaded()[0];
        assert_eq!(media.stream_type, StreamType::Live);
        assert_eq!(media.start_time, 0.0);
    }

    #[tokio::test]
    async fn rejected_token_is_refreshed_and_playback_retried_once() {
        let server = MockServer::start().await;
        let stale = fresh_token("stale");
        let fresh = fresh_token("fresh");
        Mock::given(method("GET"))
            .and(path("/v5/Playback"))
            .and(header("authorization", format!("Bearer {stale}").as_str()))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({
                "odata.error": {"code": 10000, "message": {"value": "expired"}}
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v5/RefreshAccessToken"))
            .and(header("authorization", format!("Bearer {stale}").as_str()))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"AuthToken": {"Token": fresh}})),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v5/Playback"))
            .and(header("authorization", format!("Bearer {fresh}").as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(playback_body(false, "proxy")))
            .expect(1)
            .mount(&server)
            .await;

        let session = session(&server);
        let harness = Harness::new(uhd_player());
        session
            .on_message(&harness.ctx, NS_DAZN, &init_session(&stale))
            .await;
        session
            .on_message(&harness.ctx, NS_DAZN, &init_playback("asset-1", json!(0)))
            .await;

        assert_eq!(harness.loaded().len(), 1);
        assert_eq!(
            session.state.lock().await.token.as_deref(),
            Some(fresh.as_str())
        );
    }

    #[tokio::test]
    async fn expiring_token_is_refreshed_before_playback() {
        let server = MockServer::start().await;
        let expiring = test_jwt(&json!({"exp": 1}));
        let fresh = fresh_token("fresh");
        Mock::given(method("POST"))
            .and(path("/v5/RefreshAccessToken"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"AuthToken": {"Token": fresh}})),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v5/Playback"))
            .and(header("authorization", format!("Bearer {fresh}").as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(playback_body(false, "proxy")))
            .expect(1)
            .mount(&server)
            .await;

        let session = session(&server);
        let harness = Harness::new(uhd_player());
        session
            .on_message(&harness.ctx, NS_DAZN, &init_session(&expiring))
            .await;
        session
            .on_message(&harness.ctx, NS_DAZN, &init_playback("asset-1", json!(0)))
            .await;
        assert_eq!(harness.loaded().len(), 1);
    }

    #[tokio::test]
    async fn playback_failure_is_reported_to_the_sender() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v5/Playback"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v5/RefreshAccessToken"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;

        let session = session(&server);
        let harness = Harness::new(uhd_player());
        session
            .on_message(&harness.ctx, NS_DAZN, &init_session(&fresh_token("a")))
            .await;
        session
            .on_message(&harness.ctx, NS_DAZN, &init_playback("asset-1", json!(0)))
            .await;

        assert!(harness.loaded().is_empty());
        let messages = harness.messages();
        let error = messages
            .iter()
            .find(|message| message["type"] == "ErrorOccured")
            .expect("error reported");
        assert_eq!(error["data"]["status"], 403);
        assert_eq!(
            messages.last().unwrap(),
            &json!({"type": "CurrentStateChanged", "data": "ERROR"})
        );
    }

    #[tokio::test]
    async fn playback_without_session_requires_auth() {
        let server = MockServer::start().await;
        let session = session(&server);
        let harness = Harness::new(uhd_player());
        session
            .on_message(&harness.ctx, NS_DAZN, &init_playback("asset-1", json!(0)))
            .await;
        assert!(harness.loaded().is_empty());
        assert!(harness
            .messages()
            .iter()
            .any(|message| message["type"] == "ErrorOccured" && message["data"]["status"] == 401));
    }

    #[tokio::test]
    async fn transport_messages_drive_the_playback_controller() {
        let server = MockServer::start().await;
        let session = session(&server);
        let harness = Harness::new(uhd_player());
        for message in [
            json!({"type": "Play", "data": null}),
            json!({"type": "Pause", "data": null}),
            json!({"type": "Seek", "data": 30}),
            json!({"type": "KillPlayback", "data": null}),
        ] {
            assert_eq!(
                session.on_message(&harness.ctx, NS_DAZN, &message).await,
                MessageDisposition::Handled
            );
        }
        assert_eq!(
            *harness.playback.commands.lock().unwrap(),
            vec![
                Command::Play,
                Command::Pause,
                Command::Seek(30.0),
                Command::Stop
            ]
        );
        assert_eq!(
            session
                .on_message(&harness.ctx, "urn:x-cast:other", &json!({}))
                .await,
            MessageDisposition::Unhandled
        );
    }

    #[tokio::test]
    async fn playback_updates_are_mirrored_to_the_sender() {
        let server = MockServer::start().await;
        let session = session(&server);
        session.state.lock().await.asset_id = Some("asset-1".into());
        let harness = Harness::new(uhd_player());

        session
            .on_playback_update(
                &harness.ctx,
                PlaybackState {
                    player_state: PlayerState::Playing,
                    current_time: 12.7,
                    duration: Some(5400.2),
                    idle_reason: None,
                },
            )
            .await;
        session
            .on_playback_update(
                &harness.ctx,
                PlaybackState {
                    player_state: PlayerState::Idle,
                    current_time: 0.0,
                    duration: None,
                    idle_reason: Some(IdleReason::Finished),
                },
            )
            .await;

        assert_eq!(
            harness.messages(),
            vec![
                json!({"type": "CurrentStateChanged", "data": "PLAYING"}),
                json!({
                    "type": "CurrentTimeChanged",
                    "data": {"currentDisplayTime": 12, "endDisplayTime": 5400, "timeOffset": 0.0}
                }),
                json!({"type": "CurrentStateChanged", "data": "ENDED"}),
                json!({
                    "type": "CurrentTimeChanged",
                    "data": {"currentDisplayTime": 0, "endDisplayTime": 0, "timeOffset": 0.0}
                }),
            ]
        );
        assert_eq!(session.state.lock().await.asset_id, None);
    }

    struct PassThrough;

    #[async_trait]
    impl LicenseForwarder for PassThrough {
        async fn forward(&self, request: LicenseRequest, route: LicenseRoute) -> LicenseResponse {
            let mut body = b"forwarded:".to_vec();
            body.extend(route.upstream_url.as_bytes());
            body.extend(b":");
            body.extend(request.body);
            LicenseResponse::ok(body)
        }
    }

    fn license_request() -> LicenseRequest {
        LicenseRequest {
            session_id: "sess-1".into(),
            body: vec![1, 2, 3],
            content_type: "application/octet-stream".into(),
            route_id: Some("r0".into()),
            headers: HeaderMap::new(),
        }
    }

    fn license_route(url: &str) -> LicenseRoute {
        LicenseRoute {
            route_id: "r0".into(),
            system: DrmSystem::Widevine,
            upstream_url: url.into(),
            headers: HeaderMap::new(),
        }
    }

    #[tokio::test]
    async fn proxy_mode_licenses_are_forwarded_unchanged() {
        let server = MockServer::start().await;
        let session = session(&server);
        let harness = Harness::new(uhd_player());
        let response = session
            .resolve_license(
                &harness.ctx,
                license_request(),
                license_route("https://lic.example/a"),
                &PassThrough,
            )
            .await;
        assert_eq!(response.status, 200);
        assert_eq!(
            response.body,
            b"forwarded:https://lic.example/a:\x01\x02\x03"
        );
    }

    #[tokio::test]
    async fn mpx_mode_licenses_are_wrapped_and_unwrapped() {
        let server = MockServer::start().await;
        let license_url = format!("{}/getWidevineLicense?form=json", server.uri());
        Mock::given(method("POST"))
            .and(path("/"))
            .and(query_param("token", "mpx-token"))
            .and(wiremock::matchers::body_json(json!({
                "getWidevineLicense": {"releasePid": "rp", "widevineChallenge": "AQID"}
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "getWidevineLicenseResponse": {"license": "BAUG"}
            })))
            .expect(1)
            .mount(&server)
            .await;

        let session = session(&server);
        {
            let mut state = session.state.lock().await;
            state.token = Some(fresh_token("a"));
            state.mpx_release_pid = Some((license_url.clone(), "rp".into()));
        }
        let harness = Harness::new(uhd_player());
        let response = session
            .resolve_license(
                &harness.ctx,
                license_request(),
                license_route(&license_url),
                &PassThrough,
            )
            .await;
        assert_eq!(response.status, 200);
        assert_eq!(response.body, vec![4, 5, 6]);
    }

    #[tokio::test]
    async fn mpx_license_mode_is_recorded_from_the_playback_response() {
        let init = InitPlayback {
            asset_id: "asset-1".into(),
            ..InitPlayback::default()
        };
        let response: PlaybackResponse =
            serde_json::from_value(playback_body(false, "mpx")).unwrap();
        let (_, mpx) = build_media("sess-1", &init, response).unwrap();
        assert_eq!(
            mpx,
            Some(("https://lic.example/wv?releasePid=rp".into(), "rp".into()))
        );

        let response: PlaybackResponse =
            serde_json::from_value(playback_body(false, "proxy")).unwrap();
        let (_, mpx) = build_media("sess-1", &init, response).unwrap();
        assert_eq!(mpx, None);
    }

    #[tokio::test]
    async fn plain_media_loads_are_rejected() {
        let server = MockServer::start().await;
        let session = session(&server);
        let harness = Harness::new(uhd_player());
        let request: LoadRequest = serde_json::from_value(json!({
            "requestId": 1,
            "media": {"contentId": "x", "contentType": "", "streamType": "BUFFERED"}
        }))
        .unwrap();
        let error = session
            .resolve_media(&harness.ctx, &request)
            .await
            .unwrap_err();
        assert_eq!(error.reason(), "INVALID_REQUEST");
    }
}
