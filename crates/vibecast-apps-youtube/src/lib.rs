//! Bundled YouTube app using the captured MDX/Lounge control flow.

#![forbid(unsafe_code)]

mod lounge;
mod resolver;
mod sponsorblock;

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{mpsc, watch};
use vibecast_sdk::{
    AppContext, AppManifest, AppProvider, AppSession, AppSettingsReader, AppSettingsSchema,
    ChoiceOption, LaunchCredentials, LaunchError, LoadRequest, MediaResolveError,
    MessageDisposition, PlaybackController, PlaybackMedia, PlaybackState, SettingDescriptor,
    SettingScope,
};

use lounge::{LoungeCommand, LoungeConnection, LoungeIdentity};
use resolver::{PreferredVideoCodec, ResolveError, Resolver, PREFERRED_VIDEO_CODEC_KEY};
use sponsorblock::SponsorBlock;

const APP_IDS: &[&str] = &["233637DE"];
const MDX_NAMESPACE: &str = "urn:x-cast:com.google.youtube.mdx";
const CUSTOM_DATA_NAMESPACE: &str = "urn:x-cast:com.google.cast.customdata";
const ICON_URL: &str = "https://www.gstatic.com/youtube/img/branding/favicon/favicon_144x144.png";

/// YouTube app provider.
#[derive(Debug, Default)]
pub struct YouTube;

impl YouTube {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl AppProvider for YouTube {
    fn manifest(&self) -> AppManifest {
        let mut descriptors = vec![SettingDescriptor::Choice {
            key: PREFERRED_VIDEO_CODEC_KEY.as_str().to_owned(),
            label: "Preferred video codec".to_owned(),
            description: Some(
                "Choose which video codec YouTube should prefer when available.".to_owned(),
            ),
            scope: SettingScope::AppPlayer,
            default: "auto".to_owned(),
            choices: vec![
                ChoiceOption::new("auto", "Automatic"),
                ChoiceOption::new("av1", "AV1"),
                ChoiceOption::new("vp9", "VP9"),
                ChoiceOption::new("h264", "H.264"),
            ],
        }];
        descriptors.extend(sponsorblock::setting_descriptors());
        let settings = AppSettingsSchema::with_display_name("youtube", "YouTube", descriptors)
            .expect("static YouTube settings must be valid");
        AppManifest::new("youtube", APP_IDS, "YouTube", settings)
            .with_icon_url(ICON_URL)
            .with_namespaces(&[CUSTOM_DATA_NAMESPACE, MDX_NAMESPACE])
    }

    async fn launch(
        &self,
        ctx: &AppContext,
        _credentials: LaunchCredentials,
    ) -> Result<Arc<dyn AppSession>, LaunchError> {
        let resolver = Resolver::new(ctx.http.clone());
        let playback = ctx.playback_controller();
        let capabilities = ctx.receiver.capabilities.clone();
        let sponsorblock = SponsorBlock::new(ctx.http.clone());

        let (command_tx, command_rx) = mpsc::channel(32);
        let (playback_tx, playback_rx) = mpsc::channel(32);
        let (identity_tx, identity) = watch::channel(None);
        let (cancel, _) = watch::channel(false);
        tokio::spawn(run_commands(
            command_rx,
            resolver,
            playback.clone(),
            capabilities,
            ctx.settings.clone(),
            sponsorblock.clone(),
            cancel.subscribe(),
        ));
        tokio::spawn(run_lounge(
            ctx.http.clone(),
            ctx.receiver.clone(),
            command_tx,
            playback_rx,
            identity_tx,
            cancel.subscribe(),
        ));

        Ok(Arc::new(YouTubeSession {
            resolver: Resolver::new(ctx.http.clone()),
            capabilities: ctx.receiver.capabilities.clone(),
            identity,
            playback_tx,
            playback,
            sponsorblock,
            cancel,
        }))
    }
}

struct YouTubeSession {
    resolver: Resolver,
    capabilities: vibecast_sdk::PlayerCapabilities,
    identity: watch::Receiver<Option<LoungeIdentity>>,
    playback_tx: mpsc::Sender<PlaybackState>,
    playback: Arc<dyn PlaybackController>,
    sponsorblock: SponsorBlock,
    cancel: watch::Sender<bool>,
}

#[async_trait]
impl AppSession for YouTubeSession {
    async fn resolve_media(
        &self,
        ctx: &AppContext,
        request: &LoadRequest,
    ) -> Result<PlaybackMedia, MediaResolveError> {
        let settings = ctx.settings.snapshot();
        let preferred_video_codec = PreferredVideoCodec::from_snapshot(&settings);
        let video_id = resolver::extract_video_id(&request.media.content_id)
            .ok_or_else(|| MediaResolveError::invalid_request("INVALID_YOUTUBE_VIDEO_ID"))?;
        let resolve = self.resolver.resolve(
            &video_id,
            request.current_time,
            &self.capabilities,
            preferred_video_codec,
            None,
        );
        let prepare = self.sponsorblock.prepare(&video_id, &settings);
        let (media, prepared) = tokio::join!(resolve, prepare);
        let media = media.map_err(map_resolve_error)?;
        self.sponsorblock
            .activate(prepared, request.current_time)
            .await;
        Ok(media)
    }

    async fn on_message(
        &self,
        ctx: &AppContext,
        namespace: &str,
        data: &serde_json::Value,
    ) -> MessageDisposition {
        if namespace != MDX_NAMESPACE {
            return MessageDisposition::Unhandled;
        }
        // The sender asks for the Lounge it should join; mirror the YouTube TV
        // receiver's c2n replies (`mdxSessionStatus` / `loungeToken`).
        let reply: fn(&LoungeIdentity) -> serde_json::Value =
            match data.get("type").and_then(serde_json::Value::as_str) {
                Some("getMdxSessionStatus") => mdx_session_status,
                Some("getLoungeToken") => lounge_token_message,
                _ => return MessageDisposition::Unhandled,
            };
        self.reply_with_identity(ctx, reply);
        MessageDisposition::Handled
    }

    async fn on_sender_connected(&self, ctx: &AppContext, _sender_id: &str) {
        self.reply_with_identity(ctx, mdx_session_status);
    }

    async fn on_playback_update(&self, _ctx: &AppContext, state: PlaybackState) {
        let player_state = state.player_state;
        let current_time = state.current_time;
        let _ = self.playback_tx.try_send(state);
        if let Some(target) = self
            .sponsorblock
            .skip_target(player_state, current_time)
            .await
        {
            self.playback.seek(target).await;
        }
    }

    async fn on_stop(&self, _ctx: &AppContext) {
        let _ = self.cancel.send(true);
    }
}

impl YouTubeSession {
    /// Sends `reply(identity)` on the MDX namespace once the Lounge is paired.
    fn reply_with_identity(
        &self,
        ctx: &AppContext,
        reply: fn(&LoungeIdentity) -> serde_json::Value,
    ) {
        let ctx = ctx.clone();
        let mut identity = self.identity.clone();
        let mut cancel = self.cancel.subscribe();
        tokio::spawn(async move {
            loop {
                let current_identity = { identity.borrow().clone() };
                if let Some(identity) = current_identity {
                    ctx.send_custom(MDX_NAMESPACE, reply(&identity)).await;
                    return;
                }

                tokio::select! {
                    result = identity.changed() => {
                        if result.is_err() {
                            return;
                        }
                    }
                    result = cancel.changed() => {
                        if result.is_err() || *cancel.borrow() {
                            return;
                        }
                    }
                }
            }
        });
    }
}

/// `mdxSessionStatus` as built by the YouTube TV receiver (`_.SIb`).
fn mdx_session_status(identity: &LoungeIdentity) -> serde_json::Value {
    let mut data = serde_json::json!({
        "screenId": identity.screen_id,
        "deviceId": identity.device_id,
        "loungeToken": identity.lounge_token,
    });
    if let Some(interval) = identity.refresh_interval_ms {
        data["loungeTokenRefreshIntervalMs"] = interval.into();
    }
    serde_json::json!({ "type": "mdxSessionStatus", "data": data })
}

/// `loungeToken` as built by the YouTube TV receiver (`_.RIb`).
fn lounge_token_message(identity: &LoungeIdentity) -> serde_json::Value {
    let mut data = serde_json::json!({ "loungeToken": identity.lounge_token });
    if let Some(interval) = identity.refresh_interval_ms {
        data["loungeTokenRefreshIntervalMs"] = interval.into();
    }
    serde_json::json!({ "type": "loungeToken", "data": data })
}

async fn run_lounge(
    http: reqwest::Client,
    receiver: vibecast_sdk::ReceiverContext,
    command_tx: mpsc::Sender<LoungeCommand>,
    playback_rx: mpsc::Receiver<PlaybackState>,
    identity_tx: watch::Sender<Option<LoungeIdentity>>,
    mut cancel: watch::Receiver<bool>,
) {
    let lounge = loop {
        let establish = LoungeConnection::establish(http.clone(), &receiver);
        let result = tokio::select! {
            result = establish => result,
            result = cancel.changed() => {
                if result.is_err() || *cancel.borrow() {
                    return;
                }
                continue;
            }
        };
        match result {
            Ok(lounge) => break lounge,
            Err(error) => tracing::warn!(%error, "YouTube Lounge pairing failed; retrying"),
        }
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_secs(2)) => {}
            result = cancel.changed() => {
                if result.is_err() || *cancel.borrow() {
                    return;
                }
            }
        }
    };

    lounge
        .run(command_tx, playback_rx, identity_tx, cancel)
        .await;
}

#[derive(Default)]
struct QueueState {
    video_ids: Vec<String>,
    current_index: usize,
    list_id: Option<String>,
    ctt: Option<String>,
    next_pending: bool,
}

async fn run_commands(
    mut commands: mpsc::Receiver<LoungeCommand>,
    resolver: Resolver,
    playback: Arc<dyn PlaybackController>,
    capabilities: vibecast_sdk::PlayerCapabilities,
    settings: AppSettingsReader,
    sponsorblock: SponsorBlock,
    mut cancel: watch::Receiver<bool>,
) {
    let mut queue = QueueState::default();
    loop {
        let command = tokio::select! {
            result = cancel.changed() => {
                if result.is_err() || *cancel.borrow() {
                    return;
                }
                continue;
            }
            command = commands.recv() => {
                let Some(command) = command else { return; };
                command
            }
        };

        let load = match command {
            LoungeCommand::SetPlaylist {
                video_ids,
                current_index,
                current_time,
                list_id,
                ctt,
                ..
            } => {
                queue.video_ids = video_ids;
                queue.ctt = ctt;
                queue.current_index = current_index.min(queue.video_ids.len().saturating_sub(1));
                queue.list_id = list_id;
                queue.next_pending = false;
                queue
                    .video_ids
                    .get(queue.current_index)
                    .cloned()
                    .map(|video_id| (video_id, current_time))
            }
            LoungeCommand::UpdatePlaylist { video_ids, list_id } => {
                queue.video_ids = video_ids;
                queue.list_id = list_id;
                if queue.next_pending && queue.current_index + 1 < queue.video_ids.len() {
                    queue.current_index += 1;
                    queue.next_pending = false;
                    queue
                        .video_ids
                        .get(queue.current_index)
                        .cloned()
                        .map(|video_id| (video_id, 0.0))
                } else {
                    None
                }
            }
            LoungeCommand::Next => {
                if queue.current_index + 1 < queue.video_ids.len() {
                    queue.current_index += 1;
                    queue
                        .video_ids
                        .get(queue.current_index)
                        .cloned()
                        .map(|video_id| (video_id, 0.0))
                } else {
                    queue.next_pending = true;
                    None
                }
            }
            LoungeCommand::Previous => {
                if queue.current_index > 0 {
                    queue.current_index -= 1;
                    queue
                        .video_ids
                        .get(queue.current_index)
                        .cloned()
                        .map(|video_id| (video_id, 0.0))
                } else {
                    None
                }
            }
            LoungeCommand::Stop => {
                playback.stop().await;
                None
            }
            LoungeCommand::Play => {
                playback.play().await;
                None
            }
            LoungeCommand::Pause => {
                playback.pause().await;
                None
            }
            LoungeCommand::Seek(position) => {
                playback.seek(position).await;
                None
            }
        };

        if let Some((video_id, start_time)) = load {
            let snapshot = settings.snapshot();
            let preferred_video_codec = PreferredVideoCodec::from_snapshot(&snapshot);
            let resolve = resolver.resolve(
                &video_id,
                start_time,
                &capabilities,
                preferred_video_codec,
                queue.ctt.as_deref(),
            );
            let prepare = sponsorblock.prepare(&video_id, &snapshot);
            let (media, prepared) = tokio::join!(resolve, prepare);
            match media {
                Ok(media) => {
                    sponsorblock.activate(prepared, start_time).await;
                    playback.load(media).await;
                }
                Err(error) => {
                    tracing::warn!(%video_id, %error, "failed to resolve YouTube video");
                    playback.stop().await;
                }
            }
        }
    }
}

fn map_resolve_error(error: ResolveError) -> MediaResolveError {
    match error {
        ResolveError::Http(error) => error.into(),
        ResolveError::Unplayable(message) => {
            MediaResolveError::content_unavailable("YOUTUBE_UNPLAYABLE").with_message(message)
        }
        ResolveError::NoCompatibleStream => {
            MediaResolveError::content_unavailable("NO_COMPATIBLE_STREAM")
        }
        ResolveError::Protocol(message) => {
            MediaResolveError::internal("YOUTUBE_PROTOCOL").with_message(message)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use vibecast_sdk::PlayerState;

    #[derive(Default)]
    struct RecordingPlayback {
        operations: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl PlaybackController for RecordingPlayback {
        async fn load(&self, media: PlaybackMedia) {
            self.operations
                .lock()
                .unwrap()
                .push(format!("load:{}", media.content_id.unwrap_or_default()));
        }
        async fn play(&self) {
            self.operations.lock().unwrap().push("play".into());
        }
        async fn pause(&self) {
            self.operations.lock().unwrap().push("pause".into());
        }
        async fn seek(&self, position: f64) {
            self.operations
                .lock()
                .unwrap()
                .push(format!("seek:{position}"));
        }
        async fn stop(&self) {
            self.operations.lock().unwrap().push("stop".into());
        }
    }

    #[test]
    fn provider_declares_captured_identity() {
        let manifest = YouTube::new().manifest();
        assert_eq!(manifest.app_key, "youtube");
        assert_eq!(manifest.app_ids, APP_IDS);
        assert_eq!(manifest.display_name, "YouTube");
        assert_eq!(manifest.icon_url, Some(ICON_URL));
        assert!(manifest.namespaces.contains(&MDX_NAMESPACE));
        assert!(manifest.namespaces.contains(&CUSTOM_DATA_NAMESPACE));
        assert_eq!(manifest.settings.settings().len(), 11);
        assert_eq!(
            manifest.settings.settings()[0],
            SettingDescriptor::Choice {
                key: "preferred_video_codec".to_owned(),
                label: "Preferred video codec".to_owned(),
                description: Some(
                    "Choose which video codec YouTube should prefer when available.".to_owned()
                ),
                scope: SettingScope::AppPlayer,
                default: "auto".to_owned(),
                choices: vec![
                    ChoiceOption::new("auto", "Automatic"),
                    ChoiceOption::new("av1", "AV1"),
                    ChoiceOption::new("vp9", "VP9"),
                    ChoiceOption::new("h264", "H.264"),
                ],
            }
        );
        let sponsorblock_settings = &manifest.settings.settings()[1..];
        assert!(matches!(
            &sponsorblock_settings[0],
            SettingDescriptor::Boolean {
                key,
                default: false,
                scope: SettingScope::AppPlayer,
                ..
            } if key == "sponsorblock_enabled"
        ));
        assert!(matches!(
            &sponsorblock_settings[1],
            SettingDescriptor::Boolean {
                key,
                default: true,
                scope: SettingScope::AppPlayer,
                ..
            } if key == "sponsorblock_sponsor"
        ));
        assert!(sponsorblock_settings[2..].iter().all(|setting| matches!(
            setting,
            SettingDescriptor::Boolean {
                default: false,
                scope: SettingScope::AppPlayer,
                ..
            }
        )));
    }

    #[tokio::test]
    async fn lounge_controls_are_forwarded_without_media_resolution() {
        let playback = Arc::new(RecordingPlayback::default());
        let (tx, rx) = mpsc::channel(8);
        let (_cancel, cancel_rx) = watch::channel(false);
        let worker = tokio::spawn(run_commands(
            rx,
            Resolver::new(reqwest::Client::new()),
            playback.clone(),
            vibecast_sdk::PlayerCapabilities::default(),
            vibecast_sdk::AppContext::new(
                "session",
                "transport",
                APP_IDS[0],
                reqwest::Client::new(),
                vibecast_sdk::ReceiverContext::new(
                    "YouTube test",
                    "Test",
                    "test-device",
                    std::path::PathBuf::new(),
                ),
                Arc::new(vibecast_sdk::NoopSenderChannel),
            )
            .settings,
            SponsorBlock::new(reqwest::Client::new()),
            cancel_rx,
        ));

        tx.send(LoungeCommand::Pause).await.unwrap();
        tx.send(LoungeCommand::Seek(42.0)).await.unwrap();
        tx.send(LoungeCommand::Play).await.unwrap();
        tx.send(LoungeCommand::Stop).await.unwrap();
        drop(tx);
        worker.await.unwrap();

        assert_eq!(
            *playback.operations.lock().unwrap(),
            ["pause", "seek:42", "play", "stop"]
        );
    }

    struct RecordingSender(mpsc::UnboundedSender<serde_json::Value>);

    #[async_trait]
    impl vibecast_sdk::SenderChannel for RecordingSender {
        async fn send_custom(&self, _namespace: &str, data: serde_json::Value) {
            let _ = self.0.send(data);
        }
        async fn broadcast_custom(&self, _namespace: &str, data: serde_json::Value) {
            let _ = self.0.send(data);
        }
    }

    #[tokio::test]
    async fn mdx_requests_are_answered_with_the_lounge_token() {
        let (identity_tx, identity) = watch::channel(None);
        let (playback_tx, _playback_rx) = mpsc::channel(1);
        let (cancel, _) = watch::channel(false);
        let session = YouTubeSession {
            resolver: Resolver::new(reqwest::Client::new()),
            capabilities: vibecast_sdk::PlayerCapabilities::default(),
            identity,
            playback_tx,
            playback: Arc::new(RecordingPlayback::default()),
            sponsorblock: SponsorBlock::new(reqwest::Client::new()),
            cancel,
        };
        let (sent_tx, mut sent) = mpsc::unbounded_channel();
        let ctx = vibecast_sdk::AppContext::new(
            "session",
            "transport",
            APP_IDS[0],
            reqwest::Client::new(),
            vibecast_sdk::ReceiverContext::new(
                "YouTube test",
                "Test",
                "test-device",
                std::path::PathBuf::new(),
            ),
            Arc::new(RecordingSender(sent_tx)),
        );

        // Requests before pairing are answered once the Lounge identity exists.
        let status_request = serde_json::json!({"type": "getMdxSessionStatus"});
        assert_eq!(
            session
                .on_message(&ctx, MDX_NAMESPACE, &status_request)
                .await,
            MessageDisposition::Handled
        );
        identity_tx
            .send(Some(LoungeIdentity {
                screen_id: "screen".into(),
                device_id: "device".into(),
                lounge_token: "token".into(),
                refresh_interval_ms: Some(1_123_200_000),
            }))
            .unwrap();
        assert_eq!(
            sent.recv().await.unwrap(),
            serde_json::json!({
                "type": "mdxSessionStatus",
                "data": {
                    "screenId": "screen",
                    "deviceId": "device",
                    "loungeToken": "token",
                    "loungeTokenRefreshIntervalMs": 1_123_200_000u64,
                }
            })
        );

        let token_request = serde_json::json!({"type": "getLoungeToken"});
        session
            .on_message(&ctx, MDX_NAMESPACE, &token_request)
            .await;
        assert_eq!(
            sent.recv().await.unwrap(),
            serde_json::json!({
                "type": "loungeToken",
                "data": {"loungeToken": "token", "loungeTokenRefreshIntervalMs": 1_123_200_000u64}
            })
        );

        assert_eq!(
            session
                .on_message(&ctx, CUSTOM_DATA_NAMESPACE, &token_request)
                .await,
            MessageDisposition::Unhandled
        );
    }

    #[tokio::test]
    async fn playback_updates_skip_each_continuous_crossing() {
        let playback = Arc::new(RecordingPlayback::default());
        let sponsorblock = SponsorBlock::new(reqwest::Client::new());
        sponsorblock.activate_for_test(&[(10.0, 20.0)]).await;
        let (_identity_tx, identity) = watch::channel(None);
        let (playback_tx, mut playback_rx) = mpsc::channel(4);
        let (cancel, _) = watch::channel(false);
        let session = YouTubeSession {
            resolver: Resolver::new(reqwest::Client::new()),
            capabilities: vibecast_sdk::PlayerCapabilities::default(),
            identity,
            playback_tx,
            playback: playback.clone(),
            sponsorblock,
            cancel,
        };
        let ctx = vibecast_sdk::AppContext::new(
            "session",
            "transport",
            APP_IDS[0],
            reqwest::Client::new(),
            vibecast_sdk::ReceiverContext::new(
                "YouTube test",
                "Test",
                "test-device",
                std::path::PathBuf::new(),
            ),
            Arc::new(vibecast_sdk::NoopSenderChannel),
        );
        let paused = PlaybackState {
            player_state: PlayerState::Paused,
            current_time: 12.0,
            duration: Some(100.0),
            idle_reason: None,
        };

        session.on_playback_update(&ctx, paused.clone()).await;
        assert!(playback.operations.lock().unwrap().is_empty());
        assert_eq!(playback_rx.try_recv().unwrap(), paused);

        let playing_before = PlaybackState {
            player_state: PlayerState::Playing,
            current_time: 9.9,
            duration: Some(100.0),
            idle_reason: None,
        };
        let playing_inside = PlaybackState {
            current_time: 10.1,
            ..playing_before.clone()
        };
        session
            .on_playback_update(&ctx, playing_before.clone())
            .await;
        session
            .on_playback_update(&ctx, playing_inside.clone())
            .await;
        session.on_playback_update(&ctx, playing_before).await;
        session.on_playback_update(&ctx, playing_inside).await;

        assert_eq!(*playback.operations.lock().unwrap(), ["seek:20", "seek:20"]);
    }
}
