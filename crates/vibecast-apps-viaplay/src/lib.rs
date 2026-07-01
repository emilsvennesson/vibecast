//! Bundled Viaplay app.
//!
//! Redesign of the Python `apps/viaplay`: the owned [`ViaplaySession`] holds
//! its mutable auth / playback state behind a `tokio::Mutex`, typed serde
//! models replace pydantic, and device-code polling runs as a cancellable
//! `tokio::spawn` task via [`CancellationToken`].

#![forbid(unsafe_code)]

mod api;
mod models;

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::{watch, Mutex};
use tokio_util::sync::CancellationToken;
use vibecast_sdk::{
    normalize_stream_type, AppContext, AppProvider, AppSession, DrmInfo, DrmSystem,
    LaunchCredentials, LaunchError, LoadRequest, MediaResolveCode, MediaResolveError,
    MessageDisposition, PlaybackMedia, PlaybackState, PlaybackStream, PlayerState, StreamType,
};

use crate::api::{DeviceAuthInfo, SessionCheckResult, SetupParams, ViaplayApi, ViaplayError};
use crate::models::{
    AudioTrackState, SubtitleState, UserProfile, ViaplayReceiverState, ViaplayRequest,
};

const NS_VIAPLAY: &str = "urn:x-cast:tv.viaplay.chromecast";
const APP_IDS: &[&str] = &["6313CF39", "2DB7CC49"];
const ICON_URL: &str = "https://lh3.googleusercontent.com/qXqoFPVkEZBwm7f1Yo8_7Xjv8wVeqbBeI-HfbD_KHjt0aOJf5dP_kbyQKMB1stIc0HIywc__C_Qq2CKjsg";

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

/// Viaplay app provider.
#[derive(Debug, Default)]
pub struct Viaplay;

impl Viaplay {
    /// Construct the provider.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl AppProvider for Viaplay {
    fn app_ids(&self) -> &'static [&'static str] {
        APP_IDS
    }
    fn display_name(&self) -> &'static str {
        "Viaplay"
    }
    fn app_key(&self) -> &'static str {
        "viaplay"
    }
    fn icon_url(&self) -> Option<&'static str> {
        Some(ICON_URL)
    }
    fn namespaces(&self) -> &'static [&'static str] {
        &[NS_VIAPLAY]
    }
    async fn launch(
        &self,
        ctx: &AppContext,
        credentials: LaunchCredentials,
    ) -> Result<Box<dyn AppSession>, LaunchError> {
        let (auth_tx, _) = watch::channel(false);
        Ok(Box::new(ViaplaySession {
            api: ViaplayApi::new(
                ctx.http.clone(),
                ctx.receiver.device_id.clone(),
                ctx.receiver.user_agent.clone(),
            ),
            state: Arc::new(Mutex::new(ViaplayState {
                credentials_token: credentials.credentials,
                subtitle_enabled: Some(Value::Bool(true)),
                ..Default::default()
            })),
            auth_tx,
            cancel: Mutex::new(CancellationToken::new()),
            auth_timeout: Duration::from_secs(30),
        }))
    }
}

// ---------------------------------------------------------------------------
// Session state
// ---------------------------------------------------------------------------

#[derive(Default)]
struct ViaplayState {
    authenticated: bool,
    auth_pending: bool,
    user_id: String,
    profile_id: String,
    user_display_name: String,
    country_code: String,
    receiver_name: String,
    receiver_language_code: String,
    content_root: String,
    credentials_token: Option<String>,
    current_product_url: Option<String>,
    loading_product_url: Option<String>,
    subtitle_active_language_code: Option<String>,
    subtitle_enabled: Option<Value>,
    audio_active_track: Option<String>,
    stream_type: StreamType,
    playback_state: Option<PlaybackState>,
}

impl ViaplayState {
    fn setup_params(&self) -> SetupParams {
        SetupParams {
            content_root: self.content_root.clone(),
            country_code: self.country_code.clone(),
            user_id: self.user_id.clone(),
            profile_id: self.profile_id.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

/// A running Viaplay session owning its auth + playback state.
struct ViaplaySession {
    api: ViaplayApi,
    state: Arc<Mutex<ViaplayState>>,
    auth_tx: watch::Sender<bool>,
    cancel: Mutex<CancellationToken>,
    /// How long `resolve_media` waits for authentication before failing.
    auth_timeout: Duration,
}

#[async_trait]
impl AppSession for ViaplaySession {
    async fn resolve_media(
        &self,
        ctx: &AppContext,
        request: &LoadRequest,
    ) -> Result<PlaybackMedia, MediaResolveError> {
        // Wait (bounded) for authentication to complete before resolving.
        {
            let mut rx = self.auth_tx.subscribe();
            let _ = tokio::time::timeout(self.auth_timeout, rx.wait_for(|v| *v)).await;
        }

        let custom_data = request
            .custom_data
            .as_ref()
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();

        let params = {
            let mut state = self.state.lock().await;

            if !state.authenticated {
                return Err(MediaResolveError::new(
                    MediaResolveCode::AuthRequired,
                    "NOT_AUTHENTICATED",
                ));
            }

            // Product URL.
            let templated = get_string(&custom_data, "templatedproducturl")
                .or_else(|| get_string(&custom_data, "templatedProductUrl"));
            state.current_product_url = if templated.is_some() {
                templated
            } else {
                get_string(&custom_data, "producturl")
                    .or_else(|| get_string(&custom_data, "productUrl"))
            };
            state.loading_product_url = None;

            // Subtitle / audio.
            state.subtitle_active_language_code = get_string(&custom_data, "subtitleLanguageCode");
            state.audio_active_track = get_string(&custom_data, "audioTrackLanguageCode");

            if let Some(v) = custom_data.get("subtitleActive") {
                if v.is_boolean() || v.is_object() {
                    state.subtitle_enabled = Some(v.clone());
                }
            }

            state.setup_params()
        };

        let play_url = get_string(&custom_data, "playUrl")
            .or_else(|| get_string(&custom_data, "contentUrl"))
            .unwrap_or_default();
        if play_url.is_empty() {
            return Err(MediaResolveError::invalid_request("NO_PLAY_URL"));
        }

        let stream_info = self
            .api
            .fetch_stream(&play_url, &params)
            .await
            .map_err(|e| map_viaplay_error(e, "VIAPLAY_STREAM_FETCH"))?;

        let resolved_stream_type =
            normalize_stream_type(stream_info.stream_type.unwrap_or(request.media.stream_type));

        let duration = stream_info.duration.or(request.media.duration);

        let metadata = request.media.metadata.as_ref();
        let title = stream_info
            .title
            .clone()
            .or_else(|| metadata.and_then(|m| m.title.clone()));
        let subtitle = metadata.and_then(|m| m.subtitle.clone());
        let images = metadata.map(|m| m.images.clone()).unwrap_or_default();

        let drm = stream_info.drm_license_url.as_ref().map(|license_url| {
            let mut drm = DrmInfo::new(DrmSystem::Widevine, license_url.clone());
            drm.headers = self.api.request_headers();
            drm
        });

        let mut streams = Vec::new();
        let mut seen_urls: HashSet<String> = HashSet::new();
        let mut add_stream = |url: &str| {
            if !url.is_empty() && seen_urls.insert(url.to_string()) {
                streams.push(PlaybackStream {
                    url: url.to_string(),
                    content_type: stream_info.content_type.clone(),
                    drm: drm.clone(),
                });
            }
        };
        add_stream(&stream_info.url);
        for fallback_url in &stream_info.fallback_urls {
            add_stream(fallback_url);
        }

        if streams.is_empty() {
            return Err(MediaResolveError::content_unavailable("NO_STREAM_URL"));
        }

        self.state.lock().await.stream_type = resolved_stream_type;

        Ok(PlaybackMedia {
            session_id: ctx.session_id.clone(),
            streams,
            stream_type: resolved_stream_type,
            content_id: Some(request.media.content_id.clone()),
            title,
            subtitle,
            images,
            duration,
            autoplay: request.autoplay,
            start_time: request.current_time,
            custom_data: Some(Value::Object(custom_data)),
        })
    }

    async fn on_message(
        &self,
        ctx: &AppContext,
        namespace: &str,
        data: &Value,
    ) -> MessageDisposition {
        if namespace != NS_VIAPLAY {
            return MessageDisposition::Unhandled;
        }

        let request: ViaplayRequest = match serde_json::from_value(data.clone()) {
            Ok(r) => r,
            Err(_) => return MessageDisposition::Unhandled,
        };

        match request {
            ViaplayRequest::SetupInfo {
                content_root,
                country_code,
                user_id,
                profile_id,
                receiver_name,
                receiver_language_code,
            } => {
                self.handle_setup_info(
                    ctx,
                    content_root,
                    country_code,
                    user_id,
                    profile_id,
                    receiver_name,
                    receiver_language_code,
                )
                .await;
            }
            ViaplayRequest::AuthorizationDone { success } => {
                if !success {
                    return MessageDisposition::Handled;
                }
                let api = self.api.clone();
                let state = self.state.clone();
                let auth_tx = self.auth_tx.clone();
                let ctx = ctx.clone();
                tokio::spawn(async move {
                    complete_device_auth(&api, &state, &auth_tx, &ctx).await;
                });
            }
            ViaplayRequest::GotoIdle {} => {
                {
                    let mut state = self.state.lock().await;
                    state.playback_state = None;
                }
                broadcast_receiver_state(&self.state, ctx, "IDLE").await;
            }
        }

        MessageDisposition::Handled
    }

    async fn on_sender_connected(&self, ctx: &AppContext, _sender_id: &str) {
        let status = {
            let state = self.state.lock().await;
            let player_state = state
                .playback_state
                .as_ref()
                .map(|s| s.player_state)
                .unwrap_or(PlayerState::Idle);
            receiver_status_from_player_state(player_state)
        };
        broadcast_receiver_state(&self.state, ctx, status).await;
    }

    async fn on_playback_update(&self, ctx: &AppContext, state: PlaybackState) {
        let stream_type = {
            let mut guard = self.state.lock().await;
            guard.playback_state = Some(state.clone());
            guard.stream_type
        };

        let status = receiver_status_from_player_state(state.player_state);
        broadcast_receiver_state(&self.state, ctx, status).await;

        if status == "CASTING" {
            broadcast_posdur(&self.state, ctx, &state, stream_type).await;
        }
    }

    async fn on_stop(&self, _ctx: &AppContext) {
        self.cancel.lock().await.cancel();
    }
}

impl ViaplaySession {
    #[allow(clippy::too_many_arguments)]
    async fn handle_setup_info(
        &self,
        ctx: &AppContext,
        content_root: String,
        country_code: String,
        user_id: String,
        profile_id: String,
        receiver_name: String,
        receiver_language_code: String,
    ) {
        let credentials_token = {
            let mut state = self.state.lock().await;
            state.user_id = user_id;
            state.profile_id = profile_id;
            state.country_code = country_code;
            if !receiver_name.is_empty() {
                state.receiver_name = receiver_name;
            }
            state.receiver_language_code = receiver_language_code;
            state.content_root = content_root;

            state.authenticated = false;
            state.auth_pending = false;
            state.user_display_name.clear();

            state.credentials_token.clone()
        };

        // Reset auth signal.
        let _ = self.auth_tx.send(false);

        // Cancel previous auth tasks and create a new token.
        let new_token = {
            let mut cancel = self.cancel.lock().await;
            cancel.cancel();
            let t = CancellationToken::new();
            *cancel = t.clone();
            t
        };

        let api = self.api.clone();
        let state = self.state.clone();
        let auth_tx = self.auth_tx.clone();
        let ctx = ctx.clone();
        tokio::spawn(async move {
            if let Err(e) =
                run_auth_flow(&api, &state, &auth_tx, &ctx, &new_token, credentials_token).await
            {
                tracing::error!(error = %e, "auth flow failed");
                let _ = auth_tx.send(true);
            }
        });
    }
}

// ---------------------------------------------------------------------------
// Auth flow (runs in a spawned task)
// ---------------------------------------------------------------------------

async fn run_auth_flow(
    api: &ViaplayApi,
    state: &Arc<Mutex<ViaplayState>>,
    auth_tx: &watch::Sender<bool>,
    ctx: &AppContext,
    cancel: &CancellationToken,
    credentials_token: Option<String>,
) -> Result<(), ViaplayError> {
    let params = state.lock().await.setup_params();
    let user_id = params.user_id.clone();

    let result = api.check_session(&params).await?;

    // Strategy 1: session cookie already valid.
    if result.user.as_ref().is_some_and(|u| u.user_id == user_id) {
        let user = result.user.as_ref().unwrap();
        mark_authenticated(state, auth_tx, &user.first_name, &user.last_name).await;
        send_session_ok(state, ctx).await;
        return Ok(());
    }

    // Strategy 2: persistent login.
    if let Some(url) = &result.persistent_login_url {
        if api.persistent_login(url, &params).await.unwrap_or(false) {
            let recheck = api.check_session(&params).await?;
            if recheck.user.as_ref().is_some_and(|u| u.user_id == user_id) {
                let user = recheck.user.as_ref().unwrap();
                mark_authenticated(state, auth_tx, &user.first_name, &user.last_name).await;
                send_session_ok(state, ctx).await;
                return Ok(());
            }
        }
    }

    // Strategy 3: token login.
    let token = credentials_token.unwrap_or_default();
    if let Some(url) = &result.token_login_url {
        if !token.is_empty() && api.token_login(url, &token, &params).await.unwrap_or(false) {
            let recheck = api.check_session(&params).await?;
            if recheck.user.as_ref().is_some_and(|u| u.user_id == user_id) {
                let user = recheck.user.as_ref().unwrap();
                mark_authenticated(state, auth_tx, &user.first_name, &user.last_name).await;
                send_session_ok(state, ctx).await;
                return Ok(());
            }
        }
    }

    // Strategy 4: device-code authorization.
    start_device_auth(api, state, auth_tx, ctx, cancel, Some(&result)).await
}

async fn start_device_auth(
    api: &ViaplayApi,
    state: &Arc<Mutex<ViaplayState>>,
    auth_tx: &watch::Sender<bool>,
    ctx: &AppContext,
    cancel: &CancellationToken,
    root_result: Option<&SessionCheckResult>,
) -> Result<(), ViaplayError> {
    let params = state.lock().await.setup_params();
    let auth_info = api.get_device_authorization(&params, root_result).await?;
    state.lock().await.auth_pending = true;

    let rs = build_receiver_state(
        state,
        "AUTHORIZATION_REQUIRED",
        Some(&auth_info.activate_url),
        Some(&auth_info.user_code),
    )
    .await;
    let rs_value = serde_json::to_value(&rs).expect("receiver state serialization");

    // Broadcast AUTHORIZATION_REQUIRED.
    let mut auth_msg = serde_json::Map::new();
    auth_msg.insert("type".into(), json!("AUTHORIZATION_REQUIRED"));
    if !auth_info.activate_url.is_empty() {
        auth_msg.insert("authorizationUrl".into(), json!(auth_info.activate_url));
    }
    auth_msg.insert("receiverState".into(), rs_value.clone());
    ctx.broadcast_custom(NS_VIAPLAY, Value::Object(auth_msg))
        .await;

    // Broadcast RECEIVER_STATE.
    ctx.broadcast_custom(
        NS_VIAPLAY,
        json!({"type": "RECEIVER_STATE", "receiverState": rs_value}),
    )
    .await;

    // Spawn polling task.
    let api = api.clone();
    let state = state.clone();
    let auth_tx = auth_tx.clone();
    let ctx = ctx.clone();
    let cancel = cancel.clone();
    tokio::spawn(async move {
        poll_for_authorization(&api, &state, &auth_tx, &ctx, &cancel, &auth_info).await;
    });

    Ok(())
}

async fn poll_for_authorization(
    api: &ViaplayApi,
    state: &Arc<Mutex<ViaplayState>>,
    auth_tx: &watch::Sender<bool>,
    ctx: &AppContext,
    cancel: &CancellationToken,
    auth_info: &DeviceAuthInfo,
) {
    let timeout = Duration::from_secs(300);
    let interval = Duration::from_secs(3);
    let start = tokio::time::Instant::now();

    while start.elapsed() < timeout {
        if state.lock().await.authenticated {
            return;
        }

        tokio::select! {
            () = cancel.cancelled() => return,
            () = tokio::time::sleep(interval) => {}
        }

        let params = state.lock().await.setup_params();
        match api.poll_authorized(auth_info, &params).await {
            Ok(true) => {
                complete_device_auth(api, state, auth_tx, ctx).await;
                if state.lock().await.authenticated {
                    return;
                }
            }
            Ok(false) => {}
            Err(e) => {
                tracing::debug!(error = %e, "poll authorized error");
            }
        }
    }

    // Timeout.
    state.lock().await.auth_pending = false;
    tracing::warn!("device auth timed out after 300s");
}

async fn complete_device_auth(
    api: &ViaplayApi,
    state: &Arc<Mutex<ViaplayState>>,
    auth_tx: &watch::Sender<bool>,
    ctx: &AppContext,
) {
    if state.lock().await.authenticated {
        return;
    }

    let params = state.lock().await.setup_params();
    let user_id = params.user_id.clone();
    match api.check_session(&params).await {
        Ok(result) => {
            if let Some(user) = &result.user {
                if user.user_id == user_id {
                    mark_authenticated(state, auth_tx, &user.first_name, &user.last_name).await;
                    state.lock().await.auth_pending = false;
                    send_session_ok(state, ctx).await;
                }
            }
        }
        Err(e) => {
            tracing::error!(error = %e, "complete device auth failed");
            let _ = auth_tx.send(true);
        }
    }
}

async fn mark_authenticated(
    state: &Arc<Mutex<ViaplayState>>,
    auth_tx: &watch::Sender<bool>,
    first_name: &str,
    last_name: &str,
) {
    let mut guard = state.lock().await;
    guard.authenticated = true;
    let name = format!("{first_name} {last_name}").trim().to_string();
    if !name.is_empty() {
        guard.user_display_name = name;
    }
    drop(guard);
    let _ = auth_tx.send(true);
}

async fn send_session_ok(state: &Arc<Mutex<ViaplayState>>, ctx: &AppContext) {
    let rs = build_receiver_state(state, "IDLE", None, None).await;
    let rs_value = serde_json::to_value(&rs).expect("receiver state serialization");
    let guard = state.lock().await;
    let mut msg = serde_json::Map::new();
    msg.insert("type".into(), json!("SESSION_OK"));
    if !guard.user_id.is_empty() {
        msg.insert("userId".into(), json!(guard.user_id));
    }
    if !guard.profile_id.is_empty() {
        msg.insert("profileId".into(), json!(guard.profile_id));
    }
    if !guard.user_display_name.is_empty() {
        msg.insert("userDisplayName".into(), json!(guard.user_display_name));
    }
    msg.insert("receiverState".into(), rs_value);
    drop(guard);
    ctx.broadcast_custom(NS_VIAPLAY, Value::Object(msg)).await;
}

// ---------------------------------------------------------------------------
// Receiver state helpers
// ---------------------------------------------------------------------------

async fn build_receiver_state(
    state: &Arc<Mutex<ViaplayState>>,
    status: &str,
    authorization_url: Option<&str>,
    user_code: Option<&str>,
) -> ViaplayReceiverState {
    let guard = state.lock().await;
    ViaplayReceiverState {
        status: status.to_string(),
        is_scrubbable: true,
        pne_in_progress: false,
        user_id: if guard.user_id.is_empty() {
            None
        } else {
            Some(guard.user_id.clone())
        },
        user_profile: if guard.profile_id.is_empty() {
            None
        } else {
            Some(UserProfile {
                id: Some(guard.profile_id.clone()),
                ..Default::default()
            })
        },
        user_display_name: if guard.user_display_name.is_empty() {
            None
        } else {
            Some(guard.user_display_name.clone())
        },
        country_code: guard.country_code.clone(),
        receiver_name: guard.receiver_name.clone(),
        receiver_language_code: guard.receiver_language_code.clone(),
        current_product_url: guard.current_product_url.clone(),
        loading_product_url: guard.loading_product_url.clone(),
        authorization_url: authorization_url
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string()),
        user_code: user_code.filter(|s| !s.is_empty()).map(|s| s.to_string()),
        subtitles: SubtitleState {
            active_language_code: guard.subtitle_active_language_code.clone(),
            available_language_codes: Vec::new(),
            enabled: guard.subtitle_enabled.clone(),
        },
        audio_tracks: AudioTrackState {
            active_audio_track: guard.audio_active_track.clone(),
            available_audio_tracks: Vec::new(),
        },
        intro: json!({}),
        recap: json!({}),
        tracking_debug: false,
        feature_flags: json!({}),
    }
}

async fn broadcast_receiver_state(
    state: &Arc<Mutex<ViaplayState>>,
    ctx: &AppContext,
    status: &str,
) {
    let rs = build_receiver_state(state, status, None, None).await;
    let rs_value = serde_json::to_value(&rs).expect("receiver state serialization");
    ctx.broadcast_custom(
        NS_VIAPLAY,
        json!({"type": "RECEIVER_STATE", "receiverState": rs_value}),
    )
    .await;
}

async fn broadcast_posdur(
    state: &Arc<Mutex<ViaplayState>>,
    ctx: &AppContext,
    playback_state: &PlaybackState,
    stream_type: StreamType,
) {
    let is_live = stream_type == StreamType::Live;
    let duration = playback_state.duration;

    if !is_live && (duration.is_none() || duration.unwrap_or(0.0) <= 0.0) {
        return;
    }

    let rs = build_receiver_state(state, "CASTING", None, None).await;
    let rs_value = serde_json::to_value(&rs).expect("receiver state serialization");

    let position = playback_state.current_time.max(0.0) as i64;
    let dur = duration.unwrap_or(0.0).max(0.0) as i64;

    ctx.broadcast_custom(
        NS_VIAPLAY,
        json!({
            "type": "POSDUR",
            "position": position,
            "duration": dur,
            "receiverState": rs_value,
        }),
    )
    .await;
}

fn receiver_status_from_player_state(player_state: PlayerState) -> &'static str {
    match player_state {
        PlayerState::Playing | PlayerState::Paused | PlayerState::Buffering => "CASTING",
        _ => "IDLE",
    }
}

fn get_string(data: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    match data.get(key) {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

fn map_viaplay_error(error: ViaplayError, detail: &str) -> MediaResolveError {
    match error {
        ViaplayError::NoStreamUrl => MediaResolveError::content_unavailable("NO_STREAM_URL"),
        ViaplayError::NoContentRoot | ViaplayError::NoDeviceCode | ViaplayError::Json(_) => {
            MediaResolveError::internal(detail)
        }
        ViaplayError::Http(error) => {
            let mut mapped = MediaResolveError::from(error);
            mapped.detail_code = Some(detail.to_string());
            mapped
        }
        ViaplayError::HttpStatus { status, message } => {
            MediaResolveError::from_http_status(status, Some(detail.to_string()))
                .with_message(message)
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex as StdMutex};

    use serde_json::json;
    use vibecast_sdk::{
        LicenseForwarder, LicenseRequest, LicenseResponse, LicenseRoute, MediaInfo, MediaMetadata,
        ReceiverContext, SenderChannel,
    };
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    use crate::api::ViaplayApiConfig;

    use super::*;

    // -- Test helpers -------------------------------------------------------

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

    fn context(sender: Arc<dyn SenderChannel>) -> AppContext {
        AppContext::new(
            "sess-1",
            "pid-1",
            "6313CF39",
            reqwest::Client::new(),
            ReceiverContext::new(
                "Living Room",
                "Chromecast",
                "receiver-device-id",
                PathBuf::from("/tmp/vibecast-tests/apps/viaplay"),
            ),
            sender,
        )
    }

    fn session_with(
        server: &MockServer,
        authenticated: bool,
        credentials_token: Option<String>,
    ) -> ViaplaySession {
        let config = ViaplayApiConfig {
            device_code_fallback: format!("{}/api/device/code", server.uri()),
        };
        let (auth_tx, _) = watch::channel(authenticated);
        ViaplaySession {
            api: ViaplayApi::with_config(
                reqwest::Client::new(),
                "receiver-device-id".to_string(),
                "test-user-agent".to_string(),
                config,
            ),
            state: Arc::new(Mutex::new(ViaplayState {
                authenticated,
                subtitle_enabled: Some(Value::Bool(true)),
                credentials_token,
                ..Default::default()
            })),
            auth_tx,
            cancel: Mutex::new(CancellationToken::new()),
            auth_timeout: Duration::from_millis(50),
        }
    }

    fn load(custom_data: Option<Value>) -> LoadRequest {
        LoadRequest {
            request_id: 1,
            media: MediaInfo {
                content_id: "https://placeholder".into(),
                content_type: "video/mp4".into(),
                stream_type: StreamType::Buffered,
                metadata: Some(MediaMetadata {
                    title: Some("Fallback Title".into()),
                    subtitle: Some("Episode 1".into()),
                    ..Default::default()
                }),
                duration: None,
                custom_data: None,
                content_url: None,
                media_category: None,
                start_absolute_time: None,
                is_live_media: None,
            },
            autoplay: true,
            current_time: 12.5,
            custom_data,
        }
    }

    async fn wait_for_messages(sender: &RecordingSender, count: usize) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while tokio::time::Instant::now() < deadline {
            if sender.messages.lock().unwrap().len() >= count {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    // -- Tests --------------------------------------------------------------

    #[test]
    fn app_metadata() {
        let app = Viaplay::new();
        assert_eq!(app.app_ids(), &["6313CF39", "2DB7CC49"]);
        assert_eq!(app.display_name(), "Viaplay");
        assert_eq!(app.app_key(), "viaplay");
        assert_eq!(app.namespaces(), &[NS_VIAPLAY]);
    }

    // -- Auth flow: SETUP_INFO triggers session check -----------------------

    struct SessionOkRouter;

    impl Respond for SessionOkRouter {
        fn respond(&self, request: &Request) -> ResponseTemplate {
            let path = request.url.path();
            if path.contains("chromecastgoogletv4k-") {
                return ResponseTemplate::new(200).set_body_json(json!({
                    "user": {
                        "userId": "user-1",
                        "firstName": "Test",
                        "lastName": "User",
                    }
                }));
            }
            ResponseTemplate::new(404)
        }
    }

    #[tokio::test]
    async fn setup_info_runs_auth_and_broadcasts_session_ok() {
        let server = MockServer::start().await;
        Mock::given(wiremock::matchers::any())
            .respond_with(SessionOkRouter)
            .mount(&server)
            .await;

        let recorder = RecordingSender::default();
        let ctx = context(Arc::new(recorder.clone()));
        let session = session_with(&server, false, None);

        session
            .on_message(
                &ctx,
                NS_VIAPLAY,
                &json!({
                    "type": "SETUP_INFO",
                    "contentRoot": server.uri(),
                    "countryCode": "se",
                    "userId": "user-1",
                    "profileId": "profile-1",
                }),
            )
            .await;

        wait_for_messages(&recorder, 1).await;

        let messages = recorder.messages.lock().unwrap();
        let session_ok = messages
            .iter()
            .find(|(_, v)| v["type"] == "SESSION_OK")
            .expect("SESSION_OK should be broadcast");
        assert_eq!(session_ok.0, NS_VIAPLAY);
        assert_eq!(session_ok.1["userId"], "user-1");
        assert_eq!(session_ok.1["profileId"], "profile-1");
    }

    // -- Resolve media: stream with DRM ------------------------------------

    struct StreamRouter;

    impl Respond for StreamRouter {
        fn respond(&self, request: &Request) -> ResponseTemplate {
            let path = request.url.path();
            if path.starts_with("/play/") {
                return ResponseTemplate::new(200).set_body_json(json!({
                    "duration": 3600.0,
                    "product": {
                        "content": {"title": "Live Match"},
                        "streamType": "LIVE"
                    },
                    "contentUrl": "https://cdn.example.com/manifest.mpd",
                    "contentType": "application/dash+xml",
                    "_links": {
                        "viaplay:widevineLicense": {
                            "href": "https://drm.example.com/license"
                        }
                    }
                }));
            }
            ResponseTemplate::new(404)
        }
    }

    #[tokio::test]
    async fn resolves_stream_and_returns_playback_media() {
        let server = MockServer::start().await;
        Mock::given(wiremock::matchers::any())
            .respond_with(StreamRouter)
            .mount(&server)
            .await;

        let ctx = context(Arc::new(RecordingSender::default()));
        let session = session_with(&server, true, None);

        let media = session
            .resolve_media(
                &ctx,
                &load(Some(json!({
                    "playUrl": format!("{}/play/123", server.uri())
                }))),
            )
            .await
            .expect("resolve should succeed");

        assert_eq!(media.session_id, "sess-1");
        assert_eq!(media.stream_type, StreamType::Live);
        assert_eq!(media.streams.len(), 1);
        assert_eq!(media.streams[0].url, "https://cdn.example.com/manifest.mpd");
        assert_eq!(media.streams[0].content_type, "application/dash+xml");
        assert_eq!(media.duration, Some(3600.0));
        assert_eq!(media.title.as_deref(), Some("Live Match"));
        assert_eq!(media.subtitle.as_deref(), Some("Episode 1"));
        assert_eq!(media.start_time, 12.5);

        let drm = media.streams[0].drm.as_ref().expect("DRM should be set");
        assert_eq!(drm.system, DrmSystem::Widevine);
        assert_eq!(drm.license_url, "https://drm.example.com/license");
        assert!(
            drm.headers.contains_key("Origin"),
            "DRM headers should include Origin"
        );
    }

    // -- Resolve media: auth required --------------------------------------

    #[tokio::test]
    async fn missing_auth_returns_auth_required() {
        let server = MockServer::start().await;
        let session = session_with(&server, false, None);
        // Signal that auth is "done" so we don't wait 30 s, but leave
        // authenticated = false in the state.
        let _ = session.auth_tx.send(true);

        let ctx = context(Arc::new(RecordingSender::default()));
        let result = session
            .resolve_media(
                &ctx,
                &load(Some(
                    json!({"playUrl": "https://content.viaplay.se/play/123"}),
                )),
            )
            .await;

        let error = result.unwrap_err();
        assert_eq!(error.reason(), "AUTH_REQUIRED");
        assert_eq!(error.detail_code.as_deref(), Some("NOT_AUTHENTICATED"));
    }

    // -- Playback update: RECEIVER_STATE + POSDUR --------------------------

    #[tokio::test]
    async fn playback_update_broadcasts_receiver_state_and_posdur() {
        let server = MockServer::start().await;
        let session = session_with(&server, false, None);

        let recorder = RecordingSender::default();
        let ctx = context(Arc::new(recorder.clone()));

        session
            .on_playback_update(
                &ctx,
                PlaybackState {
                    player_state: PlayerState::Playing,
                    current_time: 260.9,
                    duration: Some(2535.48),
                    idle_reason: None,
                },
            )
            .await;

        let messages = recorder.messages.lock().unwrap();
        assert_eq!(messages.len(), 2);

        let (ns1, payload1) = &messages[0];
        assert_eq!(ns1, NS_VIAPLAY);
        assert_eq!(payload1["type"], "RECEIVER_STATE");
        assert_eq!(payload1["receiverState"]["status"], "CASTING");

        let (ns2, payload2) = &messages[1];
        assert_eq!(ns2, NS_VIAPLAY);
        assert_eq!(payload2["type"], "POSDUR");
        assert_eq!(payload2["position"], 260);
        assert_eq!(payload2["duration"], 2535);
    }

    // -- Resolve license: default forwarding -------------------------------

    struct TestForwarder;

    #[async_trait]
    impl LicenseForwarder for TestForwarder {
        async fn forward(&self, _request: LicenseRequest, _route: LicenseRoute) -> LicenseResponse {
            LicenseResponse {
                body: b"license-bytes".to_vec(),
                content_type: "application/octet-stream".to_string(),
                status: 403,
            }
        }
    }

    #[tokio::test]
    async fn resolve_license_forwards_unchanged() {
        let server = MockServer::start().await;
        let session = session_with(&server, true, None);
        let ctx = context(Arc::new(RecordingSender::default()));

        let request = LicenseRequest {
            session_id: "sess-1".to_string(),
            body: b"challenge".to_vec(),
            content_type: "application/octet-stream".to_string(),
            route_id: Some("r0".to_string()),
            headers: HashMap::new(),
        };
        let route = LicenseRoute {
            route_id: "r0".to_string(),
            system: DrmSystem::Widevine,
            upstream_url: "https://drm.example.com/license".to_string(),
            headers: HashMap::new(),
        };

        let response = session
            .resolve_license(&ctx, request, route, &TestForwarder)
            .await;
        assert_eq!(response.body, b"license-bytes");
        assert_eq!(response.content_type, "application/octet-stream");
        assert_eq!(response.status, 403);
    }

    // -- Device auth: AUTHORIZATION_REQUIRED broadcast ---------------------

    struct DeviceAuthRouter {
        base: String,
    }

    impl Respond for DeviceAuthRouter {
        fn respond(&self, request: &Request) -> ResponseTemplate {
            let path = request.url.path();
            if path.contains("chromecastgoogletv4k-") {
                return ResponseTemplate::new(200).set_body_json(json!({
                    "_links": {
                        "viaplay:deviceAuthorization": {
                            "href": format!("{}/device-auth", self.base)
                        }
                    }
                }));
            }
            if path == "/device-auth" {
                return ResponseTemplate::new(200).set_body_json(json!({
                    "userCode": "ABCD",
                    "deviceToken": "token",
                    "verificationUrl": "https://viaplay.com/activate",
                    "_links": {
                        "viaplay:activate": {
                            "href": "https://viaplay.com/activate?userCode=ABCD"
                        },
                        "viaplay:authorized": {
                            "href": format!("{}/authorized", self.base)
                        }
                    }
                }));
            }
            if path == "/authorized" {
                return ResponseTemplate::new(403);
            }
            ResponseTemplate::new(404)
        }
    }

    #[tokio::test]
    async fn device_auth_emits_authorization_required() {
        let server = MockServer::start().await;
        Mock::given(wiremock::matchers::any())
            .respond_with(DeviceAuthRouter { base: server.uri() })
            .mount(&server)
            .await;

        let recorder = RecordingSender::default();
        let ctx = context(Arc::new(recorder.clone()));
        let session = session_with(&server, false, None);

        session
            .on_message(
                &ctx,
                NS_VIAPLAY,
                &json!({
                    "type": "SETUP_INFO",
                    "contentRoot": server.uri(),
                    "countryCode": "se",
                    "userId": "user-1",
                    "profileId": "profile-1",
                }),
            )
            .await;

        wait_for_messages(&recorder, 2).await;

        let messages = recorder.messages.lock().unwrap();
        let auth_msg = messages
            .iter()
            .find(|(_, v)| v["type"] == "AUTHORIZATION_REQUIRED")
            .expect("AUTHORIZATION_REQUIRED should be broadcast");
        assert_eq!(auth_msg.0, NS_VIAPLAY);
        assert_eq!(
            auth_msg.1["authorizationUrl"],
            "https://viaplay.com/activate?userCode=ABCD"
        );
    }
}
