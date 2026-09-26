//! YouTube Lounge pairing and BrowserChannel command transport.

use std::path::Path;
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio::sync::{mpsc, watch};
use url::Url;
use vibecast_sdk::{IdleReason, PlaybackState, PlayerState, ReceiverContext};

const LOUNGE_BASE: &str = "https://www.youtube.com/api/lounge";
const USER_AGENT: &str =
    "Mozilla/5.0 (Linux; Android 11) AppleWebKit/537.36 Chrome/120 Safari/537.36 CrKey/1.56";
const MAX_FRAME_LENGTH: usize = 1024 * 1024;
/// Used when the token response carries no refresh interval.
const DEFAULT_TOKEN_REFRESH: Duration = Duration::from_secs(24 * 60 * 60);
/// Floor for the server's interval, and the retry delay after a failed refresh.
const MIN_TOKEN_REFRESH: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum LoungeCommand {
    SetPlaylist {
        video_ids: Vec<String>,
        current_index: usize,
        current_time: f64,
        list_id: Option<String>,
        ctt: Option<String>,
        player_params: Option<String>,
    },
    UpdatePlaylist {
        video_ids: Vec<String>,
        list_id: Option<String>,
    },
    Play,
    Pause,
    Seek(f64),
    Next,
    Previous,
    Stop,
}

pub(crate) struct LoungeConnection {
    http: reqwest::Client,
    headers: HeaderMap,
    base: Url,
    bind_url: Url,
    bound: BoundSession,
    screen_id: String,
    device_id: String,
    lounge_token: String,
    refresh_interval_ms: Option<u64>,
    token_refresh_at: tokio::time::Instant,
    discovery_device_id: String,
    current: CurrentMedia,
    pending_incoming: Vec<Incoming>,
}

#[derive(Clone)]
pub(crate) struct LoungeIdentity {
    pub(crate) screen_id: String,
    pub(crate) device_id: String,
    pub(crate) lounge_token: String,
    pub(crate) refresh_interval_ms: Option<u64>,
}

#[derive(Clone)]
struct BoundSession {
    sid: String,
    gsession_id: String,
    aid: u64,
    rid: u64,
    ofs: u64,
}

#[derive(Default)]
struct CurrentMedia {
    video_ids: Vec<String>,
    video_id: Option<String>,
    list_id: Option<String>,
    ctt: Option<String>,
    player_params: Option<String>,
    /// Client playback nonce, fresh per video like the TV client's.
    cpn: String,
    current_index: usize,
    next_pending: bool,
    state: Option<PlaybackState>,
}

impl CurrentMedia {
    fn select(&mut self, index: usize) {
        self.current_index = index;
        self.video_id = self.video_ids.get(index).cloned();
        self.cpn = new_cpn();
    }
}

fn new_cpn() -> String {
    use base64::Engine as _;
    let bytes = uuid::Uuid::new_v4().into_bytes();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&bytes[..12])
}

#[derive(Debug, Error)]
pub(crate) enum LoungeError {
    #[error("YouTube Lounge HTTP request failed")]
    Http(#[from] reqwest::Error),
    #[error("YouTube Lounge JSON response was invalid")]
    Json(#[from] serde_json::Error),
    #[error("YouTube Lounge state could not be stored")]
    Io(#[from] std::io::Error),
    #[error("YouTube Lounge protocol error: {0}")]
    Protocol(&'static str),
}

impl LoungeError {
    /// The Lounge server refused the request (as opposed to a transient failure).
    fn is_rejection(&self) -> bool {
        match self {
            Self::Http(error) => error
                .status()
                .is_some_and(|status| status.is_client_error()),
            Self::Protocol(_) => true,
            Self::Json(_) | Self::Io(_) => false,
        }
    }
}

impl LoungeConnection {
    pub(crate) async fn establish(
        http: reqwest::Client,
        receiver: &ReceiverContext,
    ) -> Result<Self, LoungeError> {
        Self::establish_at(http, receiver, LOUNGE_BASE).await
    }

    async fn establish_at(
        http: reqwest::Client,
        receiver: &ReceiverContext,
        base: &str,
    ) -> Result<Self, LoungeError> {
        let base = Url::parse(base).map_err(|_| LoungeError::Protocol("invalid base URL"))?;
        // Like a real TV, keep one screen per receiver so a returning phone finds
        // the same Lounge; only the token is refreshed.
        let screen_path = receiver
            .data_dir
            .join(format!("lounge-{}.json", receiver.device_id));
        let stored = load_screen(&screen_path);
        let stored_token = match &stored {
            Some(screen) => match lounge_token(&http, &base, &screen.screen_id).await {
                Ok(token) => Some((screen.clone(), token)),
                Err(error) if error.is_rejection() => {
                    tracing::warn!(%error, "stored YouTube screen rejected; pairing a new one");
                    None
                }
                Err(error) => return Err(error),
            },
            None => None,
        };
        let (screen, token) = match stored_token {
            Some(paired) => paired,
            None => {
                let screen = generate_screen(&http, &base).await?;
                let token = lounge_token(&http, &base, &screen.screen_id).await?;
                (screen, token)
            }
        };
        if stored.as_ref() != Some(&screen) {
            if let Err(error) = save_screen(&screen_path, &screen) {
                tracing::warn!(%error, "failed to persist YouTube screen");
            }
        }
        let lounge_token = token.lounge_token;
        let device_id = screen.device_id.clone();
        let discovery_device_id = cast_cloud_device_id(&receiver.device_id);
        let bind_url = build_bind_url(
            &base,
            &screen.screen_id_secret,
            &lounge_token,
            &device_id,
            receiver,
        )?;
        let headers = lounge_headers(receiver, &discovery_device_id);
        let (bound, pending_incoming) = initial_bind(&http, &headers, &bind_url).await?;

        Ok(Self {
            http,
            headers,
            base,
            bind_url,
            bound,
            screen_id: screen.screen_id,
            device_id,
            lounge_token,
            refresh_interval_ms: token.refresh_interval_ms,
            token_refresh_at: token_refresh_at(token.refresh_interval_ms),
            discovery_device_id,
            current: CurrentMedia::default(),
            pending_incoming,
        })
    }

    pub(crate) fn identity(&self) -> LoungeIdentity {
        LoungeIdentity {
            screen_id: self.screen_id.clone(),
            device_id: self.device_id.clone(),
            lounge_token: self.lounge_token.clone(),
            refresh_interval_ms: self.refresh_interval_ms,
        }
    }

    /// Serves the Lounge until cancelled, publishing the identity (and every
    /// refreshed lounge token) on `identity_tx`.
    pub(crate) async fn run(
        mut self,
        command_tx: mpsc::Sender<LoungeCommand>,
        mut playback_rx: mpsc::Receiver<PlaybackState>,
        identity_tx: watch::Sender<Option<LoungeIdentity>>,
        mut cancel: watch::Receiver<bool>,
    ) {
        let _ = identity_tx.send(Some(self.identity()));
        loop {
            if *cancel.borrow() {
                return;
            }

            match self
                .run_bound(&command_tx, &mut playback_rx, &identity_tx, &mut cancel)
                .await
            {
                Ok(()) => return,
                Err(error) => tracing::warn!(%error, "YouTube Lounge session interrupted"),
            }

            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(2)) => {}
                result = cancel.changed() => {
                    if result.is_err() || *cancel.borrow() {
                        return;
                    }
                }
            }

            match initial_bind(&self.http, &self.headers, &self.bind_url).await {
                Ok((bound, pending)) => {
                    self.bound = bound;
                    self.pending_incoming = pending;
                }
                Err(error) => {
                    tracing::warn!(%error, "YouTube Lounge rebind failed");
                }
            }
        }
    }

    async fn run_bound(
        &mut self,
        command_tx: &mpsc::Sender<LoungeCommand>,
        playback_rx: &mut mpsc::Receiver<PlaybackState>,
        identity_tx: &watch::Sender<Option<LoungeIdentity>>,
        cancel: &mut watch::Receiver<bool>,
    ) -> Result<(), LoungeError> {
        // Same opening batch as the Shield.
        self.post(&[
            Outbound::AutoplayMode,
            Outbound::NowPlaying,
            Outbound::NowPlayingShorts,
            Outbound::DiscoveryDeviceId,
        ])
        .await?;

        // The server delivers loungeStatus/getNowPlaying/getDiscoveryDeviceId inside the
        // bind response and never resends them, so drain those before the poll loop.
        for incoming in std::mem::take(&mut self.pending_incoming) {
            if self.dispatch(&incoming, command_tx).await?.is_break() {
                return Ok(());
            }
        }

        loop {
            let poll = poll_commands(&self.http, &self.headers, &self.bind_url, &self.bound);
            tokio::select! {
                () = tokio::time::sleep_until(self.token_refresh_at) => {
                    self.refresh_token(identity_tx).await;
                }
                result = cancel.changed() => {
                    if result.is_err() || *cancel.borrow() {
                        return Ok(());
                    }
                }
                state = playback_rx.recv() => {
                    let Some(state) = state else { return Ok(()); };
                    let outbound = self.handle_playback(state);
                    self.post(&outbound).await?;
                }
                result = poll => {
                    let batch = match result {
                        Ok(batch) => batch,
                        Err(LoungeError::Http(error)) if error.is_timeout() => continue,
                        Err(error) => return Err(error),
                    };
                    self.bound.aid = self.bound.aid.max(batch.aid);
                    for incoming in batch.messages {
                        if self.dispatch(&incoming, command_tx).await?.is_break() {
                            return Ok(());
                        }
                    }
                }
            }
        }
    }

    /// Renews the lounge token before it expires, as the TV receiver does, so a
    /// long-lived session stays joinable and can still rebind. The current bind
    /// keeps working; the new token is used from the next (re)bind on.
    async fn refresh_token(&mut self, identity_tx: &watch::Sender<Option<LoungeIdentity>>) {
        match lounge_token(&self.http, &self.base, &self.screen_id).await {
            Ok(token) => {
                set_query_param(&mut self.bind_url, "loungeIdToken", &token.lounge_token);
                self.lounge_token = token.lounge_token;
                self.refresh_interval_ms = token.refresh_interval_ms;
                self.token_refresh_at = token_refresh_at(token.refresh_interval_ms);
                let _ = identity_tx.send(Some(self.identity()));
                tracing::debug!("refreshed YouTube lounge token");
            }
            Err(error) => {
                tracing::warn!(%error, "YouTube lounge token refresh failed; retrying");
                self.token_refresh_at = tokio::time::Instant::now() + MIN_TOKEN_REFRESH;
            }
        }
    }

    async fn dispatch(
        &mut self,
        incoming: &Incoming,
        command_tx: &mpsc::Sender<LoungeCommand>,
    ) -> Result<std::ops::ControlFlow<()>, LoungeError> {
        let outbound = self.handle_internal(incoming);
        if !outbound.is_empty() {
            self.post(&outbound).await?;
        }
        if let Incoming::Command(command) = incoming {
            if command_tx.send(command.clone()).await.is_err() {
                return Ok(std::ops::ControlFlow::Break(()));
            }
        }
        Ok(std::ops::ControlFlow::Continue(()))
    }

    fn handle_internal(&mut self, incoming: &Incoming) -> Vec<Outbound> {
        match incoming {
            Incoming::Command(LoungeCommand::SetPlaylist {
                video_ids,
                current_index,
                current_time,
                list_id,
                ctt,
                player_params,
            }) => {
                self.current.video_ids.clone_from(video_ids);
                self.current.select(*current_index);
                self.current.list_id.clone_from(list_id);
                self.current.ctt.clone_from(ctt);
                self.current.player_params.clone_from(player_params);
                self.current.next_pending = false;
                self.current.state = Some(PlaybackState {
                    player_state: PlayerState::Buffering,
                    current_time: *current_time,
                    duration: None,
                    idle_reason: None,
                });
                vec![Outbound::HasPreviousNext, Outbound::NowPlaying]
            }
            Incoming::Command(LoungeCommand::UpdatePlaylist { video_ids, list_id }) => {
                self.current.video_ids.clone_from(video_ids);
                self.current.list_id.clone_from(list_id);
                if self.current.next_pending
                    && self.current.current_index + 1 < self.current.video_ids.len()
                {
                    self.current.select(self.current.current_index + 1);
                    self.current.next_pending = false;
                    return vec![Outbound::HasPreviousNext, Outbound::NowPlaying];
                }
                vec![Outbound::HasPreviousNext]
            }
            Incoming::Command(LoungeCommand::Next) => {
                if self.current.current_index + 1 < self.current.video_ids.len() {
                    self.current.select(self.current.current_index + 1);
                    vec![Outbound::HasPreviousNext, Outbound::NowPlaying]
                } else {
                    self.current.next_pending = true;
                    Vec::new()
                }
            }
            Incoming::Command(LoungeCommand::Previous) => {
                if self.current.current_index > 0 {
                    self.current.select(self.current.current_index - 1);
                    vec![Outbound::HasPreviousNext, Outbound::NowPlaying]
                } else {
                    Vec::new()
                }
            }
            Incoming::GetNowPlaying => vec![Outbound::NowPlaying, Outbound::NowPlayingShorts],
            Incoming::GetPlaybackSpeed => vec![Outbound::PlaybackSpeed],
            Incoming::GetVolume => vec![Outbound::Volume],
            Incoming::GetPartyGamesMode => vec![Outbound::PartyGamesMode],
            Incoming::SetDiscoveryDeviceId => vec![Outbound::DiscoveryDeviceId],
            // The Shield answers a remote joining with its queue state and
            // castMatchResolved, which ties the Lounge to the Cast session.
            Incoming::RemoteConnected => {
                vec![Outbound::HasPreviousNext, Outbound::CastMatchResolved]
            }
            Incoming::Command(_) | Incoming::Ignored => Vec::new(),
        }
    }

    fn handle_playback(&mut self, state: PlaybackState) -> Vec<Outbound> {
        // Stopped (or failed) playback is abandoned: like the TV receiver, clear
        // nowPlaying so the remote drops its Now playing view. FINISHED keeps it,
        // since the remote may still advance the queue.
        let abandoned = state.player_state == PlayerState::Idle
            && matches!(
                state.idle_reason,
                Some(IdleReason::Cancelled | IdleReason::Error)
            );
        self.current.state = Some(state.clone());
        if abandoned && self.current.video_id.take().is_some() {
            vec![Outbound::State(state), Outbound::NowPlaying]
        } else {
            vec![Outbound::State(state)]
        }
    }

    async fn post(&mut self, batch: &[Outbound]) -> Result<(), LoungeError> {
        self.bound.rid += 1;
        let mut url = self.bind_url.clone();
        append_bound_query(&mut url, &self.bound, self.bound.rid.to_string().as_str());
        url.query_pairs_mut()
            .append_pair("zx", &uuid::Uuid::new_v4().simple().to_string());

        let body = form_body(
            batch,
            self.bound.ofs,
            &self.current,
            &self.discovery_device_id,
            &self.device_id,
        );
        self.bound.ofs += batch.len() as u64;
        self.http
            .post(url)
            .headers(self.headers.clone())
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(body)
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?;
        Ok(())
    }
}

enum Outbound {
    AutoplayMode,
    NowPlaying,
    NowPlayingShorts,
    State(PlaybackState),
    HasPreviousNext,
    CastMatchResolved,
    PlaybackSpeed,
    Volume,
    PartyGamesMode,
    DiscoveryDeviceId,
}

/// Encodes `batch` as one BrowserChannel POST body (`count`, `ofs`, `reqN_*`).
fn form_body(
    batch: &[Outbound],
    ofs: u64,
    current: &CurrentMedia,
    discovery_id: &str,
    lounge_device_id: &str,
) -> String {
    let mut form = url::form_urlencoded::Serializer::new(String::new());
    form.append_pair("count", &batch.len().to_string())
        .append_pair("ofs", &ofs.to_string());
    for (index, outbound) in batch.iter().enumerate() {
        outbound.append(
            &mut form,
            &format!("req{index}_"),
            current,
            discovery_id,
            lounge_device_id,
        );
    }
    form.finish()
}

impl Outbound {
    fn append(
        &self,
        form: &mut url::form_urlencoded::Serializer<'_, String>,
        prefix: &str,
        current: &CurrentMedia,
        discovery_id: &str,
        lounge_device_id: &str,
    ) {
        let key = |name: &str| format!("{prefix}{name}");
        match self {
            // vibecast plays only what the remote queues; it never autoplays.
            Self::AutoplayMode => {
                form.append_pair(&key("_sc"), "onAutoplayModeChanged")
                    .append_pair(&key("autoplayMode"), "UNSUPPORTED");
            }
            Self::NowPlaying => {
                form.append_pair(&key("_sc"), "nowPlaying");
                let Some(video_id) = &current.video_id else {
                    return;
                };
                form.append_pair(&key("videoId"), video_id);
                if let Some(state) = &current.state {
                    append_state_fields(form, prefix, state);
                    form.append_pair(&key("cpn"), &current.cpn);
                }
                for (name, value) in [
                    ("listId", &current.list_id),
                    ("ctt", &current.ctt),
                    ("playerParams", &current.player_params),
                ] {
                    if let Some(value) = value {
                        form.append_pair(&key(name), value);
                    }
                }
                form.append_pair(&key("currentIndex"), &current.current_index.to_string());
            }
            Self::NowPlayingShorts => {
                form.append_pair(&key("_sc"), "nowPlayingShorts");
            }
            Self::State(state) => {
                form.append_pair(&key("_sc"), "onStateChange");
                append_state_fields(form, prefix, state);
                form.append_pair(&key("cpn"), &current.cpn)
                    .append_pair(&key("playabilityStatus"), "OK");
            }
            Self::HasPreviousNext => {
                let has_next = current.current_index + 1 < current.video_ids.len();
                form.append_pair(&key("_sc"), "onHasPreviousNextChanged")
                    .append_pair(&key("hasPrevious"), bool_str(current.current_index > 0))
                    .append_pair(&key("hasNext"), bool_str(has_next));
            }
            Self::CastMatchResolved => {
                form.append_pair(&key("_sc"), "castMatchResolved");
            }
            Self::PlaybackSpeed => {
                form.append_pair(&key("_sc"), "onPlaybackSpeedChanged")
                    .append_pair(&key("playbackSpeed"), "1")
                    .append_pair(
                        &key("playbackSpeedInfo"),
                        r#"{"playbackSpeed":1,"isContentSupportVSP":false}"#,
                    );
            }
            Self::Volume => {
                form.append_pair(&key("_sc"), "onVolumeChanged")
                    .append_pair(&key("volume"), "100")
                    .append_pair(&key("muted"), "false");
            }
            Self::PartyGamesMode => {
                form.append_pair(&key("_sc"), "onPartyGamesModeChanged")
                    .append_pair(&key("isActive"), "false");
            }
            Self::DiscoveryDeviceId => {
                form.append_pair(&key("_sc"), "setDiscoveryDeviceId")
                    .append_pair(&key("discoveryDeviceId"), discovery_id)
                    .append_pair(&key("loungeDeviceId"), lounge_device_id)
                    .append_pair(&key("castCloudDeviceId"), discovery_id);
            }
        }
    }
}

fn bool_str(value: bool) -> &'static str {
    if value {
        "true"
    } else {
        "false"
    }
}

fn append_state_fields(
    form: &mut url::form_urlencoded::Serializer<'_, String>,
    prefix: &str,
    state: &PlaybackState,
) {
    let lounge_state = match state.player_state {
        PlayerState::Playing => "1",
        PlayerState::Paused => "2",
        PlayerState::Buffering => "3",
        PlayerState::Idle => "0",
    };
    form.append_pair(&format!("{prefix}state"), lounge_state)
        .append_pair(
            &format!("{prefix}currentTime"),
            &state.current_time.to_string(),
        )
        .append_pair(
            &format!("{prefix}duration"),
            &state.duration.unwrap_or_default().to_string(),
        )
        .append_pair(
            &format!("{prefix}loadedTime"),
            &state.current_time.to_string(),
        )
        .append_pair(&format!("{prefix}seekableStartTime"), "0")
        .append_pair(
            &format!("{prefix}seekableEndTime"),
            &state.duration.unwrap_or_default().to_string(),
        );
}

async fn initial_bind(
    http: &reqwest::Client,
    headers: &HeaderMap,
    bind_url: &Url,
) -> Result<(BoundSession, Vec<Incoming>), LoungeError> {
    let mut url = bind_url.clone();
    url.query_pairs_mut()
        .append_pair("RID", "1")
        .append_pair("CVER", "1")
        .append_pair("TYPE", "xmlhttp")
        .append_pair("zx", &uuid::Uuid::new_v4().simple().to_string());
    let bytes = http
        .post(url)
        .headers(headers.clone())
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body("count=0")
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    parse_initial_bind(&bytes)
}

fn parse_initial_bind(bytes: &[u8]) -> Result<(BoundSession, Vec<Incoming>), LoungeError> {
    let frames = decode_frames(bytes)?;
    let mut sid = None;
    let mut gsession_id = None;
    let mut aid = 0;
    let mut messages = Vec::new();
    for frame in frames {
        let Some(entries) = frame.as_array() else {
            continue;
        };
        for entry in entries {
            let Some(parts) = entry.as_array() else {
                continue;
            };
            aid = aid.max(parts.first().and_then(Value::as_u64).unwrap_or_default());
            let Some(message) = parts.get(1).and_then(Value::as_array) else {
                continue;
            };
            match message.first().and_then(Value::as_str) {
                Some("c") => sid = message.get(1).and_then(Value::as_str).map(str::to_string),
                Some("S") => {
                    gsession_id = message.get(1).and_then(Value::as_str).map(str::to_string)
                }
                // The server embeds initial control messages (loungeStatus, getNowPlaying,
                // getDiscoveryDeviceId) in the bind response and never resends them.
                _ => messages.push(parse_message(message)),
            }
        }
    }
    let bound = BoundSession {
        sid: sid.ok_or(LoungeError::Protocol("initial bind omitted SID"))?,
        gsession_id: gsession_id.ok_or(LoungeError::Protocol("initial bind omitted gsessionid"))?,
        aid,
        rid: 1,
        ofs: 0,
    };
    Ok((bound, messages))
}

async fn poll_commands(
    http: &reqwest::Client,
    headers: &HeaderMap,
    bind_url: &Url,
    bound: &BoundSession,
) -> Result<IncomingBatch, LoungeError> {
    let mut url = bind_url.clone();
    append_bound_query(&mut url, bound, "rpc");
    url.query_pairs_mut()
        .append_pair("CI", "1")
        .append_pair("TYPE", "xmlhttp")
        .append_pair("zx", &uuid::Uuid::new_v4().simple().to_string());
    let bytes = http
        .get(url)
        .headers(headers.clone())
        .timeout(Duration::from_secs(60))
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    parse_incoming(&bytes)
}

fn append_bound_query(url: &mut Url, bound: &BoundSession, rid: &str) {
    url.query_pairs_mut()
        .append_pair("RID", rid)
        .append_pair("SID", &bound.sid)
        .append_pair("AID", &bound.aid.to_string())
        .append_pair("gsessionid", &bound.gsession_id);
}

struct IncomingBatch {
    aid: u64,
    messages: Vec<Incoming>,
}

enum Incoming {
    Command(LoungeCommand),
    GetNowPlaying,
    GetPlaybackSpeed,
    GetVolume,
    GetPartyGamesMode,
    SetDiscoveryDeviceId,
    RemoteConnected,
    Ignored,
}

fn parse_incoming(bytes: &[u8]) -> Result<IncomingBatch, LoungeError> {
    let mut aid = 0;
    let mut messages = Vec::new();
    for frame in decode_frames(bytes)? {
        let Some(entries) = frame.as_array() else {
            continue;
        };
        for entry in entries {
            let Some(parts) = entry.as_array() else {
                continue;
            };
            aid = aid.max(parts.first().and_then(Value::as_u64).unwrap_or_default());
            let Some(message) = parts.get(1).and_then(Value::as_array) else {
                continue;
            };
            messages.push(parse_message(message));
        }
    }
    Ok(IncomingBatch { aid, messages })
}

fn parse_message(message: &[Value]) -> Incoming {
    let Some(name) = message.first().and_then(Value::as_str) else {
        return Incoming::Ignored;
    };
    let params = message.get(1);
    let command = match name {
        "setPlaylist" => parse_set_playlist(params),
        "updatePlaylist" => parse_update_playlist(params),
        "play" => Some(LoungeCommand::Play),
        "pause" => Some(LoungeCommand::Pause),
        "next" => Some(LoungeCommand::Next),
        "previous" => Some(LoungeCommand::Previous),
        "stopVideo" => Some(LoungeCommand::Stop),
        // Matches the TV client: parseFloat(currentTime || newTime), clamping
        // NaN/negative to 0 (`b=isNaN(b)||b<0?void 0:b;this.player.seekTo(b||0)`).
        "seekTo" => Some(LoungeCommand::Seek(parse_seek_time(params))),
        _ => None,
    };
    if let Some(command) = command {
        return Incoming::Command(command);
    }
    match name {
        "getNowPlaying" => Incoming::GetNowPlaying,
        "getPlaybackSpeed" => Incoming::GetPlaybackSpeed,
        "getVolume" => Incoming::GetVolume,
        "getPartyGamesMode" => Incoming::GetPartyGamesMode,
        "getDiscoveryDeviceId" => Incoming::SetDiscoveryDeviceId,
        "remoteConnected" => Incoming::RemoteConnected,
        _ => Incoming::Ignored,
    }
}

fn parse_set_playlist(params: Option<&Value>) -> Option<LoungeCommand> {
    let params = params?;
    let event_video_id = params
        .get("eventDetails")
        .and_then(Value::as_str)
        .and_then(|json| serde_json::from_str::<Value>(json).ok())
        .and_then(|event| event.get("videoId")?.as_str().map(str::to_string));
    let primary = params
        .get("videoId")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or(event_video_id);
    let mut video_ids = parse_video_ids(params);
    if video_ids.is_empty() {
        video_ids.extend(primary);
    }
    if video_ids.is_empty() {
        return None;
    }
    let current_index = params
        .get("currentIndex")
        .and_then(value_as_usize)
        .unwrap_or_default()
        .min(video_ids.len() - 1);
    Some(LoungeCommand::SetPlaylist {
        video_ids,
        current_index,
        current_time: params
            .get("currentTime")
            .and_then(value_as_f64)
            .unwrap_or_default(),
        list_id: string_param(params, "listId"),
        ctt: string_param(params, "ctt"),
        player_params: string_param(params, "playerParams"),
    })
}

fn string_param(params: &Value, key: &str) -> Option<String> {
    params.get(key).and_then(Value::as_str).map(str::to_string)
}

fn parse_update_playlist(params: Option<&Value>) -> Option<LoungeCommand> {
    let params = params?;
    let video_ids = parse_video_ids(params);
    (!video_ids.is_empty()).then(|| LoungeCommand::UpdatePlaylist {
        video_ids,
        list_id: params
            .get("listId")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

fn parse_seek_time(params: Option<&Value>) -> f64 {
    let time = params
        .and_then(|value| value.get("currentTime").or_else(|| value.get("newTime")))
        .and_then(value_as_f64);
    match time {
        Some(time) if time >= 0.0 => time,
        _ => 0.0,
    }
}

fn parse_video_ids(params: &Value) -> Vec<String> {
    params
        .get("videoIds")
        .and_then(Value::as_str)
        .into_iter()
        .flat_map(|ids| ids.split(','))
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .collect()
}

fn value_as_f64(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
}

fn value_as_usize(value: &Value) -> Option<usize> {
    value
        .as_u64()
        .and_then(|value| usize::try_from(value).ok())
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
}

fn decode_frames(bytes: &[u8]) -> Result<Vec<Value>, LoungeError> {
    let mut decoder = FrameDecoder::default();
    decoder.push(bytes);
    let mut frames = Vec::new();
    while let Some(frame) = decoder.next()? {
        frames.push(frame);
    }
    if !decoder.is_empty() {
        return Err(LoungeError::Protocol("truncated BrowserChannel frame"));
    }
    Ok(frames)
}

#[derive(Default)]
struct FrameDecoder {
    buffer: Vec<u8>,
}

impl FrameDecoder {
    fn push(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    fn next(&mut self) -> Result<Option<Value>, LoungeError> {
        while matches!(self.buffer.first(), Some(b'\n' | b'\r' | b' ' | b'\t')) {
            self.buffer.remove(0);
        }
        let Some(newline) = self.buffer.iter().position(|byte| *byte == b'\n') else {
            return Ok(None);
        };
        let length = std::str::from_utf8(&self.buffer[..newline])
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .ok_or(LoungeError::Protocol("invalid BrowserChannel length"))?;
        if length > MAX_FRAME_LENGTH {
            return Err(LoungeError::Protocol("BrowserChannel frame is too large"));
        }
        let payload_start = newline + 1;
        let payload_end = payload_start + length;
        if self.buffer.len() < payload_end {
            return Ok(None);
        }
        let value = serde_json::from_slice(&self.buffer[payload_start..payload_end])?;
        self.buffer.drain(..payload_end);
        Ok(Some(value))
    }

    fn is_empty(&self) -> bool {
        self.buffer
            .iter()
            .all(|byte| matches!(byte, b'\n' | b'\r' | b' ' | b'\t'))
    }
}

/// The persisted Lounge screen identity (`screenIdSecret` is never logged).
#[derive(Clone, PartialEq, Serialize, Deserialize)]
struct StoredScreen {
    #[serde(rename = "screenId")]
    screen_id: String,
    #[serde(rename = "screenIdSecret")]
    screen_id_secret: String,
    #[serde(rename = "deviceId")]
    device_id: String,
}

fn load_screen(path: &Path) -> Option<StoredScreen> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes)
        .inspect_err(|error| tracing::warn!(%error, "ignoring invalid stored YouTube screen"))
        .ok()
}

fn save_screen(path: &Path, screen: &StoredScreen) -> Result<(), LoungeError> {
    std::fs::write(path, serde_json::to_vec(screen)?)?;
    Ok(())
}

async fn generate_screen(http: &reqwest::Client, base: &Url) -> Result<StoredScreen, LoungeError> {
    let screen: ScreenIdResponse = http
        .get(join(base, "pairing/generate_screen_id")?)
        .query(&[("enable_screen_id_secret_generation", "true")])
        .header("User-Agent", USER_AGENT)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(StoredScreen {
        screen_id: screen.screen_id,
        screen_id_secret: screen.screen_id_secret,
        device_id: uuid::Uuid::new_v4().to_string(),
    })
}

async fn lounge_token(
    http: &reqwest::Client,
    base: &Url,
    screen_id: &str,
) -> Result<LoungeTokenScreen, LoungeError> {
    let body = {
        let mut serializer = url::form_urlencoded::Serializer::new(String::new());
        serializer.append_pair("screen_ids", screen_id);
        serializer.finish()
    };
    let response: LoungeTokenResponse = http
        .post(join(base, "pairing/get_lounge_token_batch")?)
        .header("User-Agent", USER_AGENT)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    response
        .screens
        .into_iter()
        .find(|item| item.screen_id == screen_id)
        .ok_or(LoungeError::Protocol("token response omitted screen"))
}

fn token_refresh_at(refresh_interval_ms: Option<u64>) -> tokio::time::Instant {
    let interval = refresh_interval_ms
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_TOKEN_REFRESH)
        .max(MIN_TOKEN_REFRESH);
    tokio::time::Instant::now() + interval
}

fn set_query_param(url: &mut Url, key: &str, value: &str) {
    let pairs: Vec<(String, String)> = url
        .query_pairs()
        .map(|(name, current)| {
            let current = if name == key {
                value.into()
            } else {
                current.into_owned()
            };
            (name.into_owned(), current)
        })
        .collect();
    url.query_pairs_mut().clear().extend_pairs(pairs);
}

fn build_bind_url(
    base: &Url,
    screen_secret: &str,
    lounge_token: &str,
    device_id: &str,
    receiver: &ReceiverContext,
) -> Result<Url, LoungeError> {
    let mut url = join(base, "bc/bind")?;
    let device_info = serde_json::json!({
        "brand": "vibecast",
        "model": receiver.device_model,
        "year": 0,
        "os": "Android",
        "osVersion": "11.0",
        "chipset": "",
        "clientName": "TVHTML5_CAST",
        "dialAdditionalDataSupportLevel": "unsupported",
        "mdxDialServerType": "MDX_DIAL_SERVER_TYPE_UNKNOWN"
    });
    url.query_pairs_mut()
        .append_pair("device", "LOUNGE_SCREEN")
        .append_pair("id", device_id)
        .append_pair("name", "YouTube on TV")
        .append_pair("app", "lb-v4")
        .append_pair("theme", "cl")
        // Mirrors the Cast-hosted TV receiver (captured from a SHIELD).
        .append_pair("capabilities", "dsp,dpa,ads,asw,apw,pas,dcn,dcp,drq")
        .append_pair("cst", "m")
        .append_pair("mdxVersion", "2")
        .append_pair("screenIdSecret", screen_secret)
        .append_pair("enforce_screen_id_secret_validation", "true")
        .append_pair("loungeIdToken", lounge_token)
        .append_pair("VER", "8")
        .append_pair("v", "2")
        .append_pair("t", "1")
        .append_pair("deviceInfo", &device_info.to_string())
        .append_pair("discoveryDeviceId", device_id);
    Ok(url)
}

/// Headers the Cast-hosted YouTube TV page sends on every Lounge request.
fn lounge_headers(receiver: &ReceiverContext, cast_device_id: &str) -> HeaderMap {
    let user_agent = if receiver.user_agent.is_empty() {
        USER_AGENT
    } else {
        receiver.user_agent.as_str()
    };
    let mut headers = HeaderMap::new();
    for (name, value) in [
        ("user-agent", user_agent),
        ("origin", "https://www.youtube.com"),
        ("referer", "https://www.youtube.com/tv?castv=2.0"),
        ("cast-app-id", crate::APP_IDS[0]),
        ("cast-app-device-id", cast_device_id),
        (
            "cast-device-capabilities",
            receiver.cast_device_capabilities.as_str(),
        ),
    ] {
        if let Ok(value) = HeaderValue::from_str(value) {
            if !value.is_empty() {
                headers.insert(HeaderName::from_static(name), value);
            }
        }
    }
    headers
}

fn cast_cloud_device_id(device_id: &str) -> String {
    uuid::Uuid::parse_str(device_id)
        .map(|id| id.simple().to_string().to_ascii_uppercase())
        .unwrap_or_else(|_| device_id.to_string())
}

fn join(base: &Url, path: &str) -> Result<Url, LoungeError> {
    base.join(&format!("{}/{}", base.path().trim_end_matches('/'), path))
        .map_err(|_| LoungeError::Protocol("invalid Lounge endpoint"))
}

#[derive(Deserialize)]
struct ScreenIdResponse {
    #[serde(rename = "screenId")]
    screen_id: String,
    #[serde(rename = "screenIdSecret")]
    screen_id_secret: String,
}

#[derive(Deserialize)]
struct LoungeTokenResponse {
    screens: Vec<LoungeTokenScreen>,
}

#[derive(Deserialize)]
struct LoungeTokenScreen {
    #[serde(rename = "screenId")]
    screen_id: String,
    #[serde(rename = "loungeToken")]
    lounge_token: String,
    #[serde(rename = "refreshIntervalMs", default)]
    refresh_interval_ms: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn frame(value: &str) -> String {
        format!("{}\n{}\n", value.len(), value)
    }

    #[test]
    fn decoder_retains_partial_and_uses_declared_byte_length() {
        let first = r#"[[5,["noop"]]]"#;
        let second = r#"[[6,["seekTo",{"note":"[inside]","newTime":"12"}]]]"#;
        let encoded = format!("{}{}", frame(first), frame(second));
        let split = frame(first).len() + 8;

        let mut decoder = FrameDecoder::default();
        decoder.push(&encoded.as_bytes()[..split]);
        assert_eq!(decoder.next().unwrap().unwrap()[0][0], 5);
        assert!(decoder.next().unwrap().is_none());
        decoder.push(&encoded.as_bytes()[split..]);
        assert_eq!(decoder.next().unwrap().unwrap()[0][0], 6);
        assert!(decoder.is_empty());
    }

    #[test]
    fn parses_captured_playlist_and_controls() {
        let body = [
            frame(r#"[[15,["setPlaylist",{"listId":"queue","eventDetails":"{\"videoId\":\"dQw4w9WgXcQ\"}","videoIds":"dQw4w9WgXcQ","currentIndex":"0","currentTime":"7.5"}]]]"#),
            frame(r#"[[16,["pause"]],[17,["play"]],[18,["seekTo",{"newTime":"111"}]]]"#),
        ]
        .concat();
        let batch = parse_incoming(body.as_bytes()).unwrap();

        assert_eq!(batch.aid, 18);
        assert!(matches!(
            &batch.messages[0],
            Incoming::Command(LoungeCommand::SetPlaylist {
                video_ids,
                current_time,
                ..
            }) if video_ids == &["dQw4w9WgXcQ"] && *current_time == 7.5
        ));
        assert!(matches!(
            batch.messages[1],
            Incoming::Command(LoungeCommand::Pause)
        ));
        assert!(matches!(
            batch.messages[2],
            Incoming::Command(LoungeCommand::Play)
        ));
        assert!(matches!(
            batch.messages[3],
            Incoming::Command(LoungeCommand::Seek(111.0))
        ));
    }

    #[test]
    fn seek_prefers_current_time_and_clamps_negatives() {
        // Real TV client: parseFloat(currentTime || newTime), NaN/negative -> 0.
        let with_current = parse_message(
            &serde_json::from_str::<Vec<Value>>(
                r#"["seekTo",{"currentTime":"30.5","newTime":"9"}]"#,
            )
            .unwrap(),
        );
        assert!(matches!(
            with_current,
            Incoming::Command(LoungeCommand::Seek(time)) if time == 30.5
        ));

        let fallback = parse_message(
            &serde_json::from_str::<Vec<Value>>(r#"["seekTo",{"newTime":"9"}]"#).unwrap(),
        );
        assert!(matches!(
            fallback,
            Incoming::Command(LoungeCommand::Seek(time)) if time == 9.0
        ));

        let negative = parse_message(
            &serde_json::from_str::<Vec<Value>>(r#"["seekTo",{"newTime":"-5"}]"#).unwrap(),
        );
        assert!(matches!(
            negative,
            Incoming::Command(LoungeCommand::Seek(time)) if time == 0.0
        ));
    }

    #[test]
    fn stop_and_previous_controls_are_parsed() {
        let stop = parse_message(&serde_json::from_str::<Vec<Value>>(r#"["stopVideo"]"#).unwrap());
        assert!(matches!(stop, Incoming::Command(LoungeCommand::Stop)));

        let previous =
            parse_message(&serde_json::from_str::<Vec<Value>>(r#"["previous"]"#).unwrap());
        assert!(matches!(
            previous,
            Incoming::Command(LoungeCommand::Previous)
        ));
    }

    #[test]
    fn initial_bind_requires_both_session_ids() {
        let valid = frame(r#"[[0,["c","SID","",8]],[1,["S","GSID"]]]"#);
        let (bound, _) = parse_initial_bind(valid.as_bytes()).unwrap();
        assert_eq!(bound.sid, "SID");
        assert_eq!(bound.gsession_id, "GSID");
        assert_eq!(bound.aid, 1);

        let missing = frame(r#"[[0,["c","SID","",8]]]"#);
        assert!(matches!(
            parse_initial_bind(missing.as_bytes()),
            Err(LoungeError::Protocol("initial bind omitted gsessionid"))
        ));
    }

    #[test]
    fn initial_bind_captures_embedded_control_messages() {
        let body = frame(
            r#"[[0,["c","SID","",8]],[1,["S","GSID"]],[2,["loungeStatus",{}]],[3,["getNowPlaying"]],[4,["getDiscoveryDeviceId"]]]"#,
        );
        let (bound, pending) = parse_initial_bind(body.as_bytes()).unwrap();
        assert_eq!(bound.aid, 4);
        assert!(matches!(pending[0], Incoming::Ignored));
        assert!(matches!(pending[1], Incoming::GetNowPlaying));
        assert!(matches!(pending[2], Incoming::SetDiscoveryDeviceId));
    }

    #[test]
    fn playback_state_is_encoded_for_lounge() {
        let current = CurrentMedia::default();
        let body = form_body(
            &[Outbound::State(PlaybackState {
                player_state: PlayerState::Paused,
                current_time: 42.5,
                duration: Some(120.0),
                idle_reason: None,
            })],
            3,
            &current,
            "device",
            "lounge-device",
        );
        let values: std::collections::HashMap<_, _> = url::form_urlencoded::parse(body.as_bytes())
            .into_owned()
            .collect();
        assert_eq!(
            values.get("req0__sc").map(String::as_str),
            Some("onStateChange")
        );
        assert_eq!(values.get("req0_state").map(String::as_str), Some("2"));
        assert_eq!(
            values.get("req0_currentTime").map(String::as_str),
            Some("42.5")
        );
    }

    #[test]
    fn stopped_playback_clears_now_playing() {
        let mut connection = LoungeConnection {
            http: reqwest::Client::new(),
            headers: HeaderMap::new(),
            base: Url::parse("https://example.test").unwrap(),
            bind_url: Url::parse("https://example.test/bc/bind").unwrap(),
            bound: BoundSession {
                sid: String::new(),
                gsession_id: String::new(),
                aid: 0,
                rid: 1,
                ofs: 0,
            },
            screen_id: String::new(),
            device_id: String::new(),
            lounge_token: String::new(),
            refresh_interval_ms: None,
            token_refresh_at: token_refresh_at(None),
            discovery_device_id: String::new(),
            current: CurrentMedia::default(),
            pending_incoming: Vec::new(),
        };
        connection.current.video_ids = vec!["Az9BrdBTKpo".to_string()];
        connection.current.select(0);
        let idle = |idle_reason| PlaybackState {
            player_state: PlayerState::Idle,
            current_time: 10.0,
            duration: Some(100.0),
            idle_reason,
        };

        let outbound = connection.handle_playback(idle(Some(IdleReason::Finished)));
        assert_eq!(outbound.len(), 1, "finished playback keeps nowPlaying");

        let outbound = connection.handle_playback(idle(Some(IdleReason::Cancelled)));
        let values = form(&form_body(&outbound, 0, &connection.current, "", ""));
        assert_eq!(values["count"], "2");
        assert_eq!(values["req0__sc"], "onStateChange");
        assert_eq!(values["req0_state"], "0");
        assert_eq!(values["req1__sc"], "nowPlaying");
        assert!(!values.contains_key("req1_videoId"));

        // Only the transition is announced; a repeated idle report is just state.
        let outbound = connection.handle_playback(idle(Some(IdleReason::Cancelled)));
        assert_eq!(outbound.len(), 1);
    }

    #[test]
    fn discovery_status_includes_cast_and_lounge_identities() {
        let body = form_body(
            &[Outbound::DiscoveryDeviceId],
            0,
            &CurrentMedia::default(),
            "CAST-ID",
            "lounge-id",
        );
        let values: std::collections::HashMap<_, _> = url::form_urlencoded::parse(body.as_bytes())
            .into_owned()
            .collect();
        assert_eq!(values.get("req0_discoveryDeviceId").unwrap(), "CAST-ID");
        assert_eq!(values.get("req0_castCloudDeviceId").unwrap(), "CAST-ID");
        assert_eq!(values.get("req0_loungeDeviceId").unwrap(), "lounge-id");
    }

    fn form(body: &str) -> std::collections::HashMap<String, String> {
        url::form_urlencoded::parse(body.as_bytes())
            .into_owned()
            .collect()
    }

    #[test]
    fn opening_batch_matches_the_captured_shield_post() {
        let body = form_body(
            &[
                Outbound::AutoplayMode,
                Outbound::NowPlaying,
                Outbound::NowPlayingShorts,
                Outbound::DiscoveryDeviceId,
            ],
            0,
            &CurrentMedia::default(),
            "CAST-ID",
            "lounge-id",
        );
        let values = form(&body);
        assert_eq!(values["count"], "4");
        assert_eq!(values["ofs"], "0");
        assert_eq!(values["req0__sc"], "onAutoplayModeChanged");
        assert_eq!(values["req1__sc"], "nowPlaying");
        assert!(
            !values.contains_key("req1_videoId"),
            "idle nowPlaying is empty"
        );
        assert_eq!(values["req2__sc"], "nowPlayingShorts");
        assert_eq!(values["req3__sc"], "setDiscoveryDeviceId");
        assert_eq!(values["req3_castCloudDeviceId"], "CAST-ID");
    }

    #[test]
    fn now_playing_echoes_the_remote_playlist_context() {
        // Captured SHIELD setPlaylist (ctt/playerParams shortened).
        let set_playlist = frame(
            r#"[[9,["setPlaylist",{"listId":"RQ_list","ctt":"APmki7T7","eventDetails":"{\"eventType\":\"VIDEO_ADDED\",\"videoId\":\"Az9BrdBTKpo\"}","playerParams":"YADIAQCQAgE=","videoIds":"Az9BrdBTKpo,yUbu5YuZEnw","currentIndex":"0","csn":"csn","currentTime":"0"}]]]"#,
        );
        let incoming = parse_incoming(set_playlist.as_bytes())
            .unwrap()
            .messages
            .remove(0);
        let mut connection = LoungeConnection {
            http: reqwest::Client::new(),
            headers: HeaderMap::new(),
            base: Url::parse("https://example.test").unwrap(),
            bind_url: Url::parse("https://example.test/bc/bind").unwrap(),
            bound: BoundSession {
                sid: String::new(),
                gsession_id: String::new(),
                aid: 0,
                rid: 1,
                ofs: 0,
            },
            screen_id: String::new(),
            device_id: String::new(),
            lounge_token: String::new(),
            refresh_interval_ms: None,
            token_refresh_at: token_refresh_at(None),
            discovery_device_id: String::new(),
            current: CurrentMedia::default(),
            pending_incoming: Vec::new(),
        };
        let outbound = connection.handle_internal(&incoming);
        let values = form(&form_body(&outbound, 4, &connection.current, "", ""));

        assert_eq!(values["count"], "2");
        assert_eq!(values["req0__sc"], "onHasPreviousNextChanged");
        assert_eq!(values["req0_hasPrevious"], "false");
        assert_eq!(values["req0_hasNext"], "true");
        assert_eq!(values["req1__sc"], "nowPlaying");
        assert_eq!(values["req1_videoId"], "Az9BrdBTKpo");
        assert_eq!(values["req1_state"], "3");
        assert_eq!(values["req1_listId"], "RQ_list");
        assert_eq!(values["req1_ctt"], "APmki7T7");
        assert_eq!(values["req1_playerParams"], "YADIAQCQAgE=");
        assert_eq!(values["req1_currentIndex"], "0");
        assert_eq!(values["req1_cpn"].len(), 16);

        let remote = parse_message(
            &serde_json::from_str::<Vec<Value>>(r#"["remoteConnected",{"id":"x"}]"#).unwrap(),
        );
        let outbound = connection.handle_internal(&remote);
        let values = form(&form_body(&outbound, 6, &connection.current, "", ""));
        assert_eq!(values["req0__sc"], "onHasPreviousNextChanged");
        assert_eq!(values["req1__sc"], "castMatchResolved");

        let party =
            parse_message(&serde_json::from_str::<Vec<Value>>(r#"["getPartyGamesMode"]"#).unwrap());
        let values = form(&form_body(
            &connection.handle_internal(&party),
            8,
            &connection.current,
            "",
            "",
        ));
        assert_eq!(values["req0__sc"], "onPartyGamesModeChanged");
        assert_eq!(values["req0_isActive"], "false");
    }

    #[test]
    fn cast_cloud_identity_normalizes_uuid_device_ids() {
        assert_eq!(
            cast_cloud_device_id("123e4567-e89b-12d3-a456-426614174000"),
            "123E4567E89B12D3A456426614174000"
        );
    }

    #[tokio::test]
    async fn establish_pairs_and_requires_a_valid_initial_bind() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/lounge/pairing/generate_screen_id"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "screenId": "screen-id",
                "screenIdSecret": "screen-secret"
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/lounge/pairing/get_lounge_token_batch"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "screens": [{
                    "screenId": "screen-id",
                    "loungeToken": "lounge-token",
                    "refreshIntervalMs": 1123200000
                }]
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/lounge/bc/bind"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(frame(r#"[[0,["c","SID","",8]],[1,["S","GSID"]]]"#)),
            )
            .mount(&server)
            .await;

        let data_dir =
            std::env::temp_dir().join(format!("vibecast-lounge-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&data_dir).unwrap();
        let receiver = ReceiverContext::new("Living Room", "Model", "device-1", data_dir.clone());
        let mut connection = LoungeConnection::establish_at(
            reqwest::Client::new(),
            &receiver,
            &format!("{}/api/lounge", server.uri()),
        )
        .await
        .unwrap();

        let identity = connection.identity();
        assert_eq!(identity.screen_id, "screen-id");
        assert!(!identity.device_id.is_empty());
        assert_eq!(identity.lounge_token, "lounge-token");
        assert_eq!(identity.refresh_interval_ms, Some(1_123_200_000));
        assert_eq!(connection.bound.sid, "SID");
        assert_eq!(connection.bound.gsession_id, "GSID");

        connection.bound.aid = 4;
        connection.post(&[Outbound::NowPlaying]).await.unwrap();
        assert_eq!(connection.bound.aid, 4, "forward ACK must not advance AID");

        // A second pairing reuses the persisted screen and lounge device id.
        let again = LoungeConnection::establish_at(
            reqwest::Client::new(),
            &receiver,
            &format!("{}/api/lounge", server.uri()),
        )
        .await
        .unwrap()
        .identity();
        assert_eq!(again.screen_id, identity.screen_id);
        assert_eq!(again.device_id, identity.device_id);
        let generated = server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|request| request.url.path().ends_with("generate_screen_id"))
            .count();
        assert_eq!(generated, 1);
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    async fn due_lounge_token_is_refreshed_and_published() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/lounge/pairing/get_lounge_token_batch"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "screens": [{
                    "screenId": "screen-id",
                    "loungeToken": "fresh-token",
                    "refreshIntervalMs": 1123200000
                }]
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/lounge/bc/bind"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/lounge/bc/bind"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(30)))
            .mount(&server)
            .await;

        let mut connection = LoungeConnection {
            http: reqwest::Client::new(),
            headers: HeaderMap::new(),
            base: Url::parse(&format!("{}/api/lounge", server.uri())).unwrap(),
            bind_url: Url::parse(&format!(
                "{}/api/lounge/bc/bind?device=LOUNGE_SCREEN&loungeIdToken=stale-token&VER=8",
                server.uri()
            ))
            .unwrap(),
            bound: BoundSession {
                sid: "SID".to_string(),
                gsession_id: "GSID".to_string(),
                aid: 0,
                rid: 1,
                ofs: 0,
            },
            screen_id: "screen-id".to_string(),
            device_id: "lounge-device".to_string(),
            lounge_token: "stale-token".to_string(),
            refresh_interval_ms: Some(1),
            token_refresh_at: tokio::time::Instant::now(),
            discovery_device_id: "CAST-ID".to_string(),
            current: CurrentMedia::default(),
            pending_incoming: Vec::new(),
        };
        let (command_tx, _command_rx) = mpsc::channel(1);
        let (_playback_tx, mut playback_rx) = mpsc::channel(1);
        let (identity_tx, mut identity_rx) = watch::channel(None);
        let (cancel_tx, mut cancel_rx) = watch::channel(false);
        let task = tokio::spawn(async move {
            let result = connection
                .run_bound(&command_tx, &mut playback_rx, &identity_tx, &mut cancel_rx)
                .await;
            (result, connection)
        });

        tokio::time::timeout(Duration::from_secs(2), identity_rx.changed())
            .await
            .expect("refreshed identity was not published")
            .unwrap();
        let identity = identity_rx.borrow().clone().unwrap();
        assert_eq!(identity.lounge_token, "fresh-token");
        assert_eq!(identity.refresh_interval_ms, Some(1_123_200_000));

        cancel_tx.send(true).unwrap();
        let (result, connection) = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("Lounge loop did not stop after cancellation")
            .unwrap();
        result.unwrap();
        // The next (re)bind presents the fresh token; other params are untouched.
        let query: Vec<(String, String)> = connection
            .bind_url
            .query_pairs()
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        assert_eq!(
            query,
            [
                ("device".to_string(), "LOUNGE_SCREEN".to_string()),
                ("loungeIdToken".to_string(), "fresh-token".to_string()),
                ("VER".to_string(), "8".to_string()),
            ]
        );
        assert!(connection.token_refresh_at > tokio::time::Instant::now() + MIN_TOKEN_REFRESH);
    }

    #[tokio::test]
    async fn discovery_identity_follows_the_server_request() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/lounge/bc/bind"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/lounge/bc/bind"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(frame(r#"[[5,["getDiscoveryDeviceId"]]]"#)),
            )
            .mount(&server)
            .await;

        let mut connection = LoungeConnection {
            http: reqwest::Client::new(),
            headers: HeaderMap::new(),
            base: Url::parse(&format!("{}/api/lounge", server.uri())).unwrap(),
            bind_url: Url::parse(&format!("{}/api/lounge/bc/bind", server.uri())).unwrap(),
            bound: BoundSession {
                sid: "SID".to_string(),
                gsession_id: "GSID".to_string(),
                aid: 0,
                rid: 1,
                ofs: 0,
            },
            screen_id: "screen-id".to_string(),
            device_id: "lounge-device".to_string(),
            lounge_token: "lounge-token".to_string(),
            refresh_interval_ms: None,
            token_refresh_at: token_refresh_at(None),
            discovery_device_id: "CAST-ID".to_string(),
            current: CurrentMedia::default(),
            pending_incoming: Vec::new(),
        };
        let (command_tx, _command_rx) = mpsc::channel(1);
        let (_playback_tx, mut playback_rx) = mpsc::channel(1);
        let (identity_tx, _identity_rx) = watch::channel(None);
        let (cancel_tx, mut cancel_rx) = watch::channel(false);
        let task = tokio::spawn(async move {
            connection
                .run_bound(&command_tx, &mut playback_rx, &identity_tx, &mut cancel_rx)
                .await
        });

        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if server.received_requests().await.unwrap().len() >= 3 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("discovery response was not sent");

        cancel_tx.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("Lounge loop did not stop after cancellation")
            .unwrap()
            .unwrap();

        let requests = server.received_requests().await.unwrap();
        let sequence = requests[..3]
            .iter()
            .map(|request| {
                if request.method.as_str() == "GET" {
                    "poll"
                } else if String::from_utf8_lossy(&request.body).starts_with("count=4") {
                    "opening"
                } else {
                    "setDiscoveryDeviceId"
                }
            })
            .collect::<Vec<_>>();
        assert_eq!(sequence, &["opening", "poll", "setDiscoveryDeviceId"]);
    }
}
