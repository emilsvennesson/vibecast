//! Bundled YouTube app using the captured MDX/Lounge control flow.

#![forbid(unsafe_code)]

mod lounge;
mod resolver;

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{mpsc, watch};
use vibecast_sdk::{
    AppContext, AppManifest, AppProvider, AppSession, AppSettingsReader, AppSettingsSchema,
    ChoiceOption, LaunchCredentials, LaunchError, LoadRequest, MediaResolveError,
    PlaybackController, PlaybackMedia, PlaybackState, SettingDescriptor, SettingScope,
};

use lounge::{LoungeCommand, LoungeConnection, LoungeIdentity};
use resolver::{PreferredVideoCodec, ResolveError, Resolver, PREFERRED_VIDEO_CODEC_KEY};

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
        let settings = AppSettingsSchema::with_display_name(
            "youtube",
            "YouTube",
            vec![SettingDescriptor::Choice {
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
            }],
        )
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

        let (command_tx, command_rx) = mpsc::channel(32);
        let (playback_tx, playback_rx) = mpsc::channel(32);
        let (identity_tx, identity) = watch::channel(None);
        let (cancel, _) = watch::channel(false);
        tokio::spawn(run_commands(
            command_rx,
            resolver,
            playback,
            capabilities,
            ctx.settings.clone(),
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
            cancel,
        }))
    }
}

struct YouTubeSession {
    resolver: Resolver,
    capabilities: vibecast_sdk::PlayerCapabilities,
    identity: watch::Receiver<Option<LoungeIdentity>>,
    playback_tx: mpsc::Sender<PlaybackState>,
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
        self.resolver
            .resolve(
                &video_id,
                request.current_time,
                &self.capabilities,
                preferred_video_codec,
            )
            .await
            .map_err(map_resolve_error)
    }

    async fn on_sender_connected(&self, ctx: &AppContext, _sender_id: &str) {
        let ctx = ctx.clone();
        let mut identity = self.identity.clone();
        let mut cancel = self.cancel.subscribe();
        tokio::spawn(async move {
            loop {
                let current_identity = { identity.borrow().clone() };
                if let Some(identity) = current_identity {
                    ctx.send_custom(
                        MDX_NAMESPACE,
                        serde_json::json!({
                            "type": "mdxSessionStatus",
                            "data": {
                                "screenId": identity.screen_id,
                                "deviceId": identity.device_id,
                            }
                        }),
                    )
                    .await;
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

    async fn on_playback_update(&self, _ctx: &AppContext, state: PlaybackState) {
        let _ = self.playback_tx.try_send(state);
    }

    async fn on_stop(&self, _ctx: &AppContext) {
        let _ = self.cancel.send(true);
    }
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

    let _ = identity_tx.send(Some(lounge.identity()));
    lounge.run(command_tx, playback_rx, cancel).await;
}

#[derive(Default)]
struct QueueState {
    video_ids: Vec<String>,
    current_index: usize,
    list_id: Option<String>,
    next_pending: bool,
}

async fn run_commands(
    mut commands: mpsc::Receiver<LoungeCommand>,
    resolver: Resolver,
    playback: Arc<dyn PlaybackController>,
    capabilities: vibecast_sdk::PlayerCapabilities,
    settings: AppSettingsReader,
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
            } => {
                queue.video_ids = video_ids;
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
            match resolver
                .resolve(&video_id, start_time, &capabilities, preferred_video_codec)
                .await
            {
                Ok(media) => playback.load(media).await,
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
        assert_eq!(manifest.settings.settings().len(), 1);
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
            cancel_rx,
        ));

        tx.send(LoungeCommand::Pause).await.unwrap();
        tx.send(LoungeCommand::Seek(42.0)).await.unwrap();
        tx.send(LoungeCommand::Play).await.unwrap();
        drop(tx);
        worker.await.unwrap();

        assert_eq!(
            *playback.operations.lock().unwrap(),
            ["pause", "seek:42", "play"]
        );
    }
}
