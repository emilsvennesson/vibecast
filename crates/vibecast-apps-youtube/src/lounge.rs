//! YouTube Lounge pairing and BrowserChannel command transport.

use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;
use tokio::sync::{mpsc, watch};
use url::Url;
use vibecast_sdk::{PlaybackState, PlayerState, ReceiverContext};

const LOUNGE_BASE: &str = "https://www.youtube.com/api/lounge";
const USER_AGENT: &str =
    "Mozilla/5.0 (Linux; Android 11) AppleWebKit/537.36 Chrome/120 Safari/537.36 CrKey/1.56";
const MAX_FRAME_LENGTH: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum LoungeCommand {
    SetPlaylist {
        video_ids: Vec<String>,
        current_index: usize,
        current_time: f64,
        list_id: Option<String>,
    },
    UpdatePlaylist {
        video_ids: Vec<String>,
        list_id: Option<String>,
    },
    Play,
    Pause,
    Seek(f64),
    Next,
}

pub(crate) struct LoungeConnection {
    http: reqwest::Client,
    bind_url: Url,
    bound: BoundSession,
    screen_id: String,
    device_id: String,
    discovery_device_id: String,
    current: CurrentMedia,
}

#[derive(Clone)]
pub(crate) struct LoungeIdentity {
    pub(crate) screen_id: String,
    pub(crate) device_id: String,
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
    current_index: usize,
    next_pending: bool,
    state: Option<PlaybackState>,
}

#[derive(Debug, Error)]
pub(crate) enum LoungeError {
    #[error("YouTube Lounge HTTP request failed")]
    Http(#[from] reqwest::Error),
    #[error("YouTube Lounge JSON response was invalid")]
    Json(#[from] serde_json::Error),
    #[error("YouTube Lounge protocol error: {0}")]
    Protocol(&'static str),
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
        let screen: ScreenIdResponse = http
            .get(join(&base, "pairing/generate_screen_id")?)
            .query(&[("enable_screen_id_secret_generation", "true")])
            .header("User-Agent", USER_AGENT)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        let token_body = {
            let mut serializer = url::form_urlencoded::Serializer::new(String::new());
            serializer.append_pair("screen_ids", &screen.screen_id);
            serializer.finish()
        };
        let token_response: LoungeTokenResponse = http
            .post(join(&base, "pairing/get_lounge_token_batch")?)
            .header("User-Agent", USER_AGENT)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(token_body)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let lounge_token = token_response
            .screens
            .into_iter()
            .find(|item| item.screen_id == screen.screen_id)
            .ok_or(LoungeError::Protocol("token response omitted screen"))?
            .lounge_token;

        let device_id = uuid::Uuid::new_v4().to_string();
        let bind_url = build_bind_url(
            &base,
            &screen.screen_id_secret,
            &lounge_token,
            &device_id,
            receiver,
        )?;
        let bound = initial_bind(&http, &bind_url).await?;

        Ok(Self {
            http,
            bind_url,
            bound,
            screen_id: screen.screen_id,
            device_id,
            discovery_device_id: cast_cloud_device_id(&receiver.device_id),
            current: CurrentMedia::default(),
        })
    }

    pub(crate) fn identity(&self) -> LoungeIdentity {
        LoungeIdentity {
            screen_id: self.screen_id.clone(),
            device_id: self.device_id.clone(),
        }
    }

    pub(crate) async fn run(
        mut self,
        command_tx: mpsc::Sender<LoungeCommand>,
        mut playback_rx: mpsc::Receiver<PlaybackState>,
        mut cancel: watch::Receiver<bool>,
    ) {
        loop {
            if *cancel.borrow() {
                return;
            }

            match self
                .run_bound(&command_tx, &mut playback_rx, &mut cancel)
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

            match initial_bind(&self.http, &self.bind_url).await {
                Ok(bound) => self.bound = bound,
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
        cancel: &mut watch::Receiver<bool>,
    ) -> Result<(), LoungeError> {
        self.post(Outbound::NowPlaying).await?;

        loop {
            let poll = poll_commands(&self.http, &self.bind_url, &self.bound);
            tokio::select! {
                result = cancel.changed() => {
                    if result.is_err() || *cancel.borrow() {
                        return Ok(());
                    }
                }
                state = playback_rx.recv() => {
                    let Some(state) = state else { return Ok(()); };
                    self.current.state = Some(state.clone());
                    self.post(Outbound::State(state)).await?;
                }
                result = poll => {
                    let batch = match result {
                        Ok(batch) => batch,
                        Err(LoungeError::Http(error)) if error.is_timeout() => continue,
                        Err(error) => return Err(error),
                    };
                    self.bound.aid = self.bound.aid.max(batch.aid);
                    for incoming in batch.messages {
                        if let Some(outbound) = self.handle_internal(&incoming) {
                            self.post(outbound).await?;
                        }
                        if let Incoming::Command(command) = incoming {
                            if command_tx.send(command).await.is_err() {
                                return Ok(());
                            }
                        }
                    }
                }
            }
        }
    }

    fn handle_internal(&mut self, incoming: &Incoming) -> Option<Outbound> {
        match incoming {
            Incoming::Command(LoungeCommand::SetPlaylist {
                video_ids,
                current_index,
                current_time,
                list_id,
            }) => {
                self.current.video_ids.clone_from(video_ids);
                self.current.video_id = video_ids.get(*current_index).cloned();
                self.current.current_index = *current_index;
                self.current.list_id.clone_from(list_id);
                self.current.next_pending = false;
                self.current.state = Some(PlaybackState {
                    player_state: PlayerState::Buffering,
                    current_time: *current_time,
                    duration: None,
                    idle_reason: None,
                });
                Some(Outbound::NowPlaying)
            }
            Incoming::Command(LoungeCommand::UpdatePlaylist { video_ids, list_id }) => {
                self.current.video_ids.clone_from(video_ids);
                self.current.list_id.clone_from(list_id);
                if self.current.next_pending
                    && self.current.current_index + 1 < self.current.video_ids.len()
                {
                    self.current.current_index += 1;
                    self.current.video_id = self
                        .current
                        .video_ids
                        .get(self.current.current_index)
                        .cloned();
                    self.current.next_pending = false;
                    return Some(Outbound::NowPlaying);
                }
                None
            }
            Incoming::Command(LoungeCommand::Next) => {
                if self.current.current_index + 1 < self.current.video_ids.len() {
                    self.current.current_index += 1;
                    self.current.video_id = self
                        .current
                        .video_ids
                        .get(self.current.current_index)
                        .cloned();
                    Some(Outbound::NowPlaying)
                } else {
                    self.current.next_pending = true;
                    None
                }
            }
            Incoming::GetNowPlaying => Some(Outbound::NowPlaying),
            Incoming::GetPlaybackSpeed => Some(Outbound::PlaybackSpeed),
            Incoming::GetVolume => Some(Outbound::Volume),
            Incoming::SetDiscoveryDeviceId => Some(Outbound::DiscoveryDeviceId),
            Incoming::Command(_) | Incoming::Ignored => None,
        }
    }

    async fn post(&mut self, outbound: Outbound) -> Result<(), LoungeError> {
        self.bound.rid += 1;
        let mut url = self.bind_url.clone();
        append_bound_query(&mut url, &self.bound, self.bound.rid.to_string().as_str());
        url.query_pairs_mut()
            .append_pair("zx", &uuid::Uuid::new_v4().simple().to_string());

        let body = outbound.form_body(
            self.bound.ofs,
            &self.current,
            &self.discovery_device_id,
            &self.device_id,
        );
        self.bound.ofs += 1;
        self.http
            .post(url)
            .header("User-Agent", USER_AGENT)
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
    NowPlaying,
    State(PlaybackState),
    PlaybackSpeed,
    Volume,
    DiscoveryDeviceId,
}

impl Outbound {
    fn form_body(
        &self,
        ofs: u64,
        current: &CurrentMedia,
        discovery_id: &str,
        lounge_device_id: &str,
    ) -> String {
        let mut form = url::form_urlencoded::Serializer::new(String::new());
        form.append_pair("count", "1")
            .append_pair("ofs", &ofs.to_string());
        match self {
            Self::NowPlaying => {
                form.append_pair("req0__sc", "nowPlaying");
                if let Some(video_id) = &current.video_id {
                    form.append_pair("req0_videoId", video_id);
                }
                if let Some(state) = &current.state {
                    append_state_fields(&mut form, "req0_", state);
                }
                if let Some(list_id) = &current.list_id {
                    form.append_pair("req0_listId", list_id);
                }
                form.append_pair("req0_currentIndex", &current.current_index.to_string());
            }
            Self::State(state) => {
                form.append_pair("req0__sc", "onStateChange");
                append_state_fields(&mut form, "req0_", state);
                form.append_pair("req0_playabilityStatus", "OK");
            }
            Self::PlaybackSpeed => {
                form.append_pair("req0__sc", "onPlaybackSpeedChanged")
                    .append_pair("req0_playbackSpeed", "1");
            }
            Self::Volume => {
                form.append_pair("req0__sc", "onVolumeChanged")
                    .append_pair("req0_volume", "100")
                    .append_pair("req0_muted", "false");
            }
            Self::DiscoveryDeviceId => {
                form.append_pair("req0__sc", "setDiscoveryDeviceId")
                    .append_pair("req0_discoveryDeviceId", discovery_id)
                    .append_pair("req0_loungeDeviceId", lounge_device_id)
                    .append_pair("req0_castCloudDeviceId", discovery_id);
            }
        }
        form.finish()
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

async fn initial_bind(http: &reqwest::Client, bind_url: &Url) -> Result<BoundSession, LoungeError> {
    let mut url = bind_url.clone();
    url.query_pairs_mut()
        .append_pair("RID", "1")
        .append_pair("CVER", "1")
        .append_pair("TYPE", "xmlhttp")
        .append_pair("zx", &uuid::Uuid::new_v4().simple().to_string());
    let bytes = http
        .post(url)
        .header("User-Agent", USER_AGENT)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body("count=0")
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    parse_initial_bind(&bytes)
}

fn parse_initial_bind(bytes: &[u8]) -> Result<BoundSession, LoungeError> {
    let frames = decode_frames(bytes)?;
    let mut sid = None;
    let mut gsession_id = None;
    let mut aid = 0;
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
                _ => {}
            }
        }
    }
    Ok(BoundSession {
        sid: sid.ok_or(LoungeError::Protocol("initial bind omitted SID"))?,
        gsession_id: gsession_id.ok_or(LoungeError::Protocol("initial bind omitted gsessionid"))?,
        aid,
        rid: 1,
        ofs: 0,
    })
}

async fn poll_commands(
    http: &reqwest::Client,
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
        .header("User-Agent", USER_AGENT)
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
    SetDiscoveryDeviceId,
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
        "seekTo" => params
            .and_then(|value| value.get("newTime"))
            .and_then(value_as_f64)
            .map(LoungeCommand::Seek),
        _ => None,
    };
    if let Some(command) = command {
        return Incoming::Command(command);
    }
    match name {
        "getNowPlaying" => Incoming::GetNowPlaying,
        "getPlaybackSpeed" => Incoming::GetPlaybackSpeed,
        "getVolume" => Incoming::GetVolume,
        "onSetDiscoveryDeviceId" => Incoming::SetDiscoveryDeviceId,
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
        list_id: params
            .get("listId")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
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
        "os": "Linux",
        "osVersion": "1",
        "chipset": "",
        "clientName": "TVHTML5",
        "dialAdditionalDataSupportLevel": "unsupported",
        "mdxDialServerType": "MDX_DIAL_SERVER_TYPE_UNKNOWN"
    });
    url.query_pairs_mut()
        .append_pair("device", "LOUNGE_SCREEN")
        .append_pair("id", device_id)
        .append_pair("name", "YouTube on TV")
        .append_pair("app", "lb-v4")
        .append_pair("theme", "cl")
        .append_pair(
            "capabilities",
            "dsp,dpa,mic,ntb,vsp,ads,pas,dcn,dcp,drq,sads",
        )
        .append_pair("cst", "m")
        .append_pair("mdxVersion", "2")
        .append_pair("screenIdSecret", screen_secret)
        .append_pair("loungeIdToken", lounge_token)
        .append_pair("VER", "8")
        .append_pair("v", "2")
        .append_pair("t", "1")
        .append_pair("deviceInfo", &device_info.to_string());
    Ok(url)
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
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
    fn initial_bind_requires_both_session_ids() {
        let valid = frame(r#"[[0,["c","SID","",8]],[1,["S","GSID"]]]"#);
        let bound = parse_initial_bind(valid.as_bytes()).unwrap();
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
    fn playback_state_is_encoded_for_lounge() {
        let current = CurrentMedia::default();
        let body = Outbound::State(PlaybackState {
            player_state: PlayerState::Paused,
            current_time: 42.5,
            duration: Some(120.0),
            idle_reason: None,
        })
        .form_body(3, &current, "device", "lounge-device");
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
    fn discovery_status_includes_cast_and_lounge_identities() {
        let body = Outbound::DiscoveryDeviceId.form_body(
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
                "screens": [{"screenId": "screen-id", "loungeToken": "lounge-token"}]
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

        let receiver = ReceiverContext::new("Living Room", "Model", "device-1", PathBuf::new());
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
        assert_eq!(connection.bound.sid, "SID");
        assert_eq!(connection.bound.gsession_id, "GSID");

        connection.bound.aid = 4;
        connection.post(Outbound::NowPlaying).await.unwrap();
        assert_eq!(connection.bound.aid, 4, "forward ACK must not advance AID");
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
                    .set_body_string(frame(r#"[[5,["onSetDiscoveryDeviceId"]]]"#)),
            )
            .mount(&server)
            .await;

        let mut connection = LoungeConnection {
            http: reqwest::Client::new(),
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
            discovery_device_id: "CAST-ID".to_string(),
            current: CurrentMedia::default(),
        };
        let (command_tx, _command_rx) = mpsc::channel(1);
        let (_playback_tx, mut playback_rx) = mpsc::channel(1);
        let (cancel_tx, mut cancel_rx) = watch::channel(false);
        let task = tokio::spawn(async move {
            connection
                .run_bound(&command_tx, &mut playback_rx, &mut cancel_rx)
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
                } else if String::from_utf8_lossy(&request.body).contains("req0__sc=nowPlaying") {
                    "nowPlaying"
                } else {
                    "setDiscoveryDeviceId"
                }
            })
            .collect::<Vec<_>>();
        assert_eq!(sequence, &["nowPlaying", "poll", "setDiscoveryDeviceId"]);
    }
}
