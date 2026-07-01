//! The device hub: a single-task actor owning the transport registry,
//! subscriptions, receiver-0 platform state, and all app sessions.
//!
//! Ports `vibecast._runtime.device` + `handlers` + the IO half of the
//! coordinator. Everything runs in one task fed by [`HubEvent`]s (cast messages,
//! renderer reports, and internal media-resolution results), preserving the
//! serialized semantics of the asyncio design without shared-mutable locking.
//! The one slow operation — `resolve_media` — is spawned and its result fed
//! back as a [`HubEvent::MediaResolved`], so the mailbox never blocks on it.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::mpsc;
use uuid::Uuid;

use vibecast_bridge::{PlayerCommand, PlayerReport, Renderer};
use vibecast_cast::{namespace as ns, ConnectionHandle, ServerEvent};
use vibecast_messages::{
    extract_request_id, ApplicationStatus, CastNamespace, ConnectionMessage, DeviceInfoResponse,
    GetDeviceInfoRequest, IdleReason, InvalidRequestResponse, LaunchErrorResponse, LaunchRequest,
    LoadFailedResponse, LoadRequest, MediaInvalidRequestResponse, MediaRequest,
    MultizoneGetStatusRequest, MultizoneStatusResponse, PlayerState, QueueItemIdsResponse,
    ReceiverRequest, ReceiverStatus, ReceiverStatusResponse, SetupRequest, SetupResponse, Volume,
};
use vibecast_proto::CastMessage;
use vibecast_sdk::{
    AppContext, AppSession, LaunchCredentials, MediaResolveError, MessageDisposition,
    NoopSenderChannel, PlaybackMedia, PlaybackState, ReceiverContext, SenderChannel,
};

use crate::coordinator::{loading_media_info, media_info, Coordinator};
use crate::identity::DeviceIdentity;
use crate::proxy::{collect_routes, rewrite_streams, to_payload, SessionProxy};
use crate::registry::{AppRegistry, ProxyRegistrar};

const RECEIVER_0: &str = "receiver-0";

/// A per-callback [`SenderChannel`] that writes custom-namespace app messages
/// directly to the relevant connection(s), never through the hub mailbox (which
/// is what awaits the app callback).
struct HubSender {
    transport_id: String,
    bound: Option<(ConnectionHandle, String)>,
    subscribers: Vec<ConnectionHandle>,
}

#[async_trait]
impl SenderChannel for HubSender {
    async fn send_custom(&self, namespace: &str, data: Value) {
        match &self.bound {
            Some((handle, sender_id)) => {
                let _ = handle
                    .send_json(&self.transport_id, sender_id, namespace, &data)
                    .await;
            }
            None => self.broadcast_custom(namespace, data).await,
        }
    }

    async fn broadcast_custom(&self, namespace: &str, data: Value) {
        for handle in &self.subscribers {
            let _ = handle
                .send_json(&self.transport_id, "*", namespace, &data)
                .await;
        }
    }
}

/// An event driving the hub actor.
pub enum HubEvent {
    /// A transport event from the Cast TLS server.
    Server(ServerEvent),
    /// A player report from the renderer bridge.
    Report(PlayerReport),
    /// The result of an app's `resolve_media` (internal feedback).
    MediaResolved(MediaResolved),
    /// Stop all app sessions cleanly, then acknowledge (graceful shutdown).
    Shutdown(tokio::sync::oneshot::Sender<()>),
}

/// The (spawned) result of resolving media for one LOAD request.
pub struct MediaResolved {
    session_id: String,
    request_id: i64,
    connection_id: u64,
    sender_id: String,
    result: Result<PlaybackMedia, MediaResolveError>,
}

/// A running app session registered as a Cast transport (id == transport id).
struct Session {
    app_id: String,
    app_key: String,
    display_name: String,
    icon_url: Option<String>,
    status_text: String,
    namespaces: Vec<String>,
    app: Arc<dyn AppSession>,
    ctx: AppContext,
    coordinator: Coordinator,
}

/// Construction parameters for the hub.
pub struct HubConfig {
    /// Device identity.
    pub identity: DeviceIdentity,
    /// App registry.
    pub registry: AppRegistry,
    /// Renderer commands sink (the player bridge).
    pub renderer: Arc<dyn Renderer>,
    /// Session proxy registration (the player bridge).
    pub proxy: Arc<dyn ProxyRegistrar>,
    /// Shared HTTP client for apps and the license/manifest proxy.
    pub http: reqwest::Client,
    /// Base data directory.
    pub data_dir: PathBuf,
    /// Initial receiver volume.
    pub volume: Volume,
    /// User-Agent placed in each app session's `ReceiverContext`.
    pub user_agent: String,
    /// `CAST-DEVICE-CAPABILITIES` header value for app sessions.
    pub cast_device_capabilities: String,
    /// Output display width for app sessions.
    pub display_width: u32,
    /// Output display height for app sessions.
    pub display_height: u32,
}

/// The device hub actor.
pub struct DeviceHub {
    identity: DeviceIdentity,
    registry: AppRegistry,
    renderer: Arc<dyn Renderer>,
    proxy: Arc<dyn ProxyRegistrar>,
    http: reqwest::Client,
    data_dir: PathBuf,
    volume: Volume,
    user_agent: String,
    cast_device_capabilities: String,
    display_width: u32,
    display_height: u32,
    connections: HashMap<u64, ConnectionHandle>,
    /// `(connection, sender)` -> transport id.
    subscriptions: HashMap<(u64, String), String>,
    /// session id (== transport id) -> session.
    sessions: HashMap<String, Session>,
    self_tx: mpsc::Sender<HubEvent>,
    events: Option<mpsc::Receiver<HubEvent>>,
}

impl DeviceHub {
    /// Build a hub. Feed it with [`sender`](Self::sender) and drive it with
    /// [`run`](Self::run).
    #[must_use]
    pub fn new(config: HubConfig) -> Self {
        let (tx, rx) = mpsc::channel(128);
        Self {
            identity: config.identity,
            registry: config.registry,
            renderer: config.renderer,
            proxy: config.proxy,
            http: config.http,
            data_dir: config.data_dir,
            volume: config.volume,
            user_agent: config.user_agent,
            cast_device_capabilities: config.cast_device_capabilities,
            display_width: config.display_width,
            display_height: config.display_height,
            connections: HashMap::new(),
            subscriptions: HashMap::new(),
            sessions: HashMap::new(),
            self_tx: tx,
            events: Some(rx),
        }
    }

    /// A sender for feeding [`HubEvent`]s (cast events and renderer reports).
    #[must_use]
    pub fn sender(&self) -> mpsc::Sender<HubEvent> {
        self.self_tx.clone()
    }

    /// Run the hub until the event channel closes.
    pub async fn run(mut self) {
        let mut events = self.events.take().expect("run called once");
        while let Some(event) = events.recv().await {
            self.dispatch(event).await;
        }
    }

    async fn dispatch(&mut self, event: HubEvent) {
        match event {
            HubEvent::Server(ServerEvent::Connected(handle)) => {
                self.connections.insert(handle.id(), handle);
            }
            HubEvent::Server(ServerEvent::Disconnected { id, .. }) => {
                self.subscriptions.retain(|(conn, _), _| *conn != id);
                self.connections.remove(&id);
            }
            HubEvent::Server(ServerEvent::Message { handle, message }) => {
                self.on_message(&handle, message).await;
            }
            HubEvent::Report(report) => self.on_report(report).await,
            HubEvent::MediaResolved(resolved) => self.on_media_resolved(resolved).await,
            HubEvent::Shutdown(ack) => {
                for session_id in self.sessions.keys().cloned().collect::<Vec<_>>() {
                    self.stop_session(&session_id).await;
                }
                let _ = ack.send(());
            }
        }
    }

    async fn on_message(&mut self, handle: &ConnectionHandle, message: CastMessage) {
        let destination = message.destination_id.clone();
        if destination == RECEIVER_0 {
            self.handle_platform(handle, message).await;
        } else if self.sessions.contains_key(&destination) {
            self.handle_session_message(handle, message).await;
        } else {
            tracing::debug!(dest = %destination, "message for unknown transport");
        }
    }

    // -- receiver-0 platform ------------------------------------------------

    async fn handle_platform(&mut self, handle: &ConnectionHandle, message: CastMessage) {
        let Some(payload) = parse_payload(&message) else {
            return;
        };
        let conn_id = handle.id();
        let source = message.source_id.clone();
        match message.namespace.as_str() {
            ns::CONNECTION => match serde_json::from_value::<ConnectionMessage>(payload) {
                Ok(ConnectionMessage::Connect(_)) => {
                    self.subscriptions
                        .insert((conn_id, source), RECEIVER_0.to_string());
                }
                _ => {
                    self.subscriptions.remove(&(conn_id, source));
                }
            },
            ns::RECEIVER => self.handle_receiver(conn_id, &source, payload).await,
            ns::DISCOVERY => {
                if let Ok(request) = serde_json::from_value::<GetDeviceInfoRequest>(payload) {
                    let response = DeviceInfoResponse::new(
                        request.request_id,
                        self.identity.device_id.clone(),
                        self.identity.device_model.clone(),
                        self.identity.friendly_name.clone(),
                    );
                    self.send_to(conn_id, RECEIVER_0, &source, ns::DISCOVERY, &response)
                        .await;
                }
            }
            ns::MULTIZONE => {
                if let Ok(request) = serde_json::from_value::<MultizoneGetStatusRequest>(payload) {
                    let response = MultizoneStatusResponse::empty(request.request_id);
                    self.send_to(conn_id, RECEIVER_0, &source, ns::MULTIZONE, &response)
                        .await;
                }
            }
            ns::SETUP => {
                if let Ok(request) = serde_json::from_value::<SetupRequest>(payload) {
                    let response = SetupResponse::ok(
                        request.request_id,
                        self.identity.friendly_name.clone(),
                        self.identity.ssdp_udn.clone(),
                    );
                    self.send_to(conn_id, RECEIVER_0, &source, ns::SETUP, &response)
                        .await;
                }
            }
            other => tracing::warn!(namespace = %other, "unhandled platform namespace"),
        }
    }

    async fn handle_receiver(&mut self, conn_id: u64, source: &str, payload: serde_json::Value) {
        let request = match serde_json::from_value::<ReceiverRequest>(payload.clone()) {
            Ok(request) => request,
            Err(_) => {
                let response = InvalidRequestResponse::new(
                    extract_request_id(&payload),
                    "Invalid receiver request",
                );
                self.send_to(conn_id, RECEIVER_0, source, ns::RECEIVER, &response)
                    .await;
                return;
            }
        };

        match request {
            ReceiverRequest::GetStatus(r) => {
                let response = self.receiver_status(r.request_id);
                self.send_to(conn_id, RECEIVER_0, source, ns::RECEIVER, &response)
                    .await;
            }
            ReceiverRequest::GetAppAvailability(r) => {
                let response =
                    vibecast_messages::AppAvailabilityResponse::available(r.request_id, &r.app_id);
                self.send_to(conn_id, RECEIVER_0, source, ns::RECEIVER, &response)
                    .await;
            }
            ReceiverRequest::Launch(r) => self.handle_launch(conn_id, source, r).await,
            ReceiverRequest::Stop(r) => {
                self.stop_session(&r.session_id).await;
                let response = self.receiver_status(r.request_id);
                self.broadcast(RECEIVER_0, ns::RECEIVER, &response).await;
            }
            ReceiverRequest::SetVolume(r) => {
                self.volume.apply_update(&r.volume);
                let response = self.receiver_status(r.request_id);
                self.broadcast(RECEIVER_0, ns::RECEIVER, &response).await;
            }
        }
    }

    async fn handle_launch(&mut self, conn_id: u64, source: &str, request: LaunchRequest) {
        let Some(provider) = self.registry.get(&request.app_id) else {
            let response =
                LaunchErrorResponse::new(request.request_id, "Application not available");
            self.send_to(conn_id, RECEIVER_0, source, ns::RECEIVER, &response)
                .await;
            return;
        };

        // LAUNCH replaces the current app: stop existing sessions first.
        for session_id in self.sessions.keys().cloned().collect::<Vec<_>>() {
            self.stop_session(&session_id).await;
        }

        let session_id = Uuid::new_v4().to_string();
        let (credentials, credentials_type) = request.resolved_credentials();
        let app_key = provider.app_key();
        let data_dir = self.data_dir.join("apps").join(app_key);
        let _ = std::fs::create_dir_all(&data_dir);

        // The stored session context carries only data + a no-op sender; the
        // hub builds a fresh, sender-bound context per callback that can send.
        let ctx = AppContext::new(
            session_id.clone(),
            session_id.clone(),
            request.app_id.clone(),
            self.http.clone(),
            ReceiverContext {
                friendly_name: self.identity.friendly_name.clone(),
                device_model: self.identity.device_model.clone(),
                device_id: self.identity.device_id.clone(),
                data_dir,
                user_agent: self.user_agent.clone(),
                cast_device_capabilities: self.cast_device_capabilities.clone(),
                display_width: self.display_width,
                display_height: self.display_height,
            },
            Arc::new(NoopSenderChannel),
        );

        let app: Arc<dyn AppSession> = match provider
            .launch(
                &ctx,
                LaunchCredentials {
                    credentials,
                    credentials_type,
                },
            )
            .await
        {
            Ok(session) => Arc::from(session),
            Err(error) => {
                tracing::warn!(%error, "app launch failed");
                let response =
                    LaunchErrorResponse::new(request.request_id, "Application launch failed");
                self.send_to(conn_id, RECEIVER_0, source, ns::RECEIVER, &response)
                    .await;
                return;
            }
        };

        let mut namespaces: Vec<String> = provider
            .namespaces()
            .iter()
            .filter(|name| **name != ns::MEDIA)
            .map(|name| (*name).to_string())
            .collect();
        namespaces.sort();
        namespaces.push(ns::MEDIA.to_string());

        let session = Session {
            app_id: request.app_id.clone(),
            app_key: app_key.to_string(),
            display_name: provider.display_name().to_string(),
            icon_url: provider.icon_url().map(str::to_string),
            status_text: provider.display_name().to_string(),
            namespaces,
            app,
            ctx,
            coordinator: Coordinator::new(self.volume.clone()),
        };
        self.sessions.insert(session_id, session);

        let response = self.receiver_status(request.request_id);
        self.broadcast(RECEIVER_0, ns::RECEIVER, &response).await;
    }

    async fn stop_session(&mut self, session_id: &str) {
        let Some(session) = self.sessions.remove(session_id) else {
            return;
        };
        if session.coordinator.playback_media.is_some() {
            self.renderer
                .send(PlayerCommand::Stop {
                    session_id: session_id.to_string(),
                })
                .await;
        }
        self.unregister_proxies(session_id);
        // Build the teardown context while this session's subscribers are still
        // registered, so on_stop can broadcast a final message.
        let ctx = self.callback_context(&session, None);
        session.app.on_stop(&ctx).await;
        self.subscriptions
            .retain(|_, transport| transport != session_id);
    }

    fn receiver_status(&self, request_id: i64) -> ReceiverStatusResponse {
        let applications: Vec<ApplicationStatus> = self
            .sessions
            .iter()
            .map(|(session_id, session)| {
                let sender_connected = self
                    .subscriptions
                    .values()
                    .any(|transport| transport == session_id);
                ApplicationStatus {
                    app_id: session.app_id.clone(),
                    display_name: session.display_name.clone(),
                    session_id: session_id.clone(),
                    transport_id: session_id.clone(),
                    status_text: session.status_text.clone(),
                    namespaces: session
                        .namespaces
                        .iter()
                        .map(|name| CastNamespace { name: name.clone() })
                        .collect(),
                    is_idle_screen: false,
                    app_type: Some("WEB".to_string()),
                    icon_url: session.icon_url.clone(),
                    launched_from_cloud: Some(false),
                    sender_connected: Some(sender_connected),
                    universal_app_id: Some(session.app_id.clone()),
                }
            })
            .collect();
        ReceiverStatusResponse::new(
            request_id,
            ReceiverStatus {
                applications,
                volume: self.volume.clone(),
                is_active_input: Some(true),
                is_stand_by: Some(false),
            },
        )
    }

    // -- app session transports --------------------------------------------

    async fn handle_session_message(&mut self, handle: &ConnectionHandle, message: CastMessage) {
        let Some(payload) = parse_payload(&message) else {
            return;
        };
        let conn_id = handle.id();
        let transport = message.destination_id.clone();
        let source = message.source_id.clone();

        match message.namespace.as_str() {
            ns::CONNECTION => match serde_json::from_value::<ConnectionMessage>(payload) {
                Ok(ConnectionMessage::Connect(_)) => {
                    self.subscriptions
                        .insert((conn_id, source.clone()), transport.clone());
                    let (app, ctx, response) = match self.sessions.get(&transport) {
                        Some(session) => (
                            session.app.clone(),
                            self.callback_context(session, Some((conn_id, source.clone()))),
                            session.coordinator.status_response(0),
                        ),
                        None => return,
                    };
                    self.send_to(conn_id, &transport, &source, ns::MEDIA, &response)
                        .await;
                    app.on_sender_connected(&ctx, &source).await;
                }
                _ => {
                    self.subscriptions.remove(&(conn_id, source));
                }
            },
            ns::MEDIA => {
                self.handle_media(conn_id, &transport, &source, payload)
                    .await
            }
            other => {
                let (app, ctx) = match self.sessions.get(&transport) {
                    Some(session) => (
                        session.app.clone(),
                        self.callback_context(session, Some((conn_id, source.clone()))),
                    ),
                    None => return,
                };
                let namespace = other.to_string();
                if app.on_message(&ctx, &namespace, &payload).await == MessageDisposition::Unhandled
                {
                    tracing::debug!(namespace = %namespace, "app left message unhandled");
                }
            }
        }
    }

    async fn handle_media(
        &mut self,
        conn_id: u64,
        transport: &str,
        source: &str,
        payload: serde_json::Value,
    ) {
        let request = match serde_json::from_value::<MediaRequest>(payload.clone()) {
            Ok(request) => request,
            Err(_) => {
                let response = MediaInvalidRequestResponse::new(
                    extract_request_id(&payload),
                    "Invalid media request",
                );
                self.send_to(conn_id, transport, source, ns::MEDIA, &response)
                    .await;
                return;
            }
        };

        match request {
            MediaRequest::Load(load) => self.media_load(conn_id, transport, source, load).await,
            MediaRequest::Play(r) => {
                self.transition(transport, r.request_id, |c| {
                    c.player_state = PlayerState::Playing;
                    c.idle_reason = None;
                })
                .await;
                self.renderer
                    .send(PlayerCommand::Play {
                        session_id: transport.to_string(),
                    })
                    .await;
                self.notify_app(transport).await;
            }
            MediaRequest::Pause(r) => {
                self.transition(transport, r.request_id, |c| {
                    c.player_state = PlayerState::Paused;
                    c.idle_reason = None;
                })
                .await;
                self.renderer
                    .send(PlayerCommand::Pause {
                        session_id: transport.to_string(),
                    })
                    .await;
                self.notify_app(transport).await;
            }
            MediaRequest::Seek(r) => {
                let position = r.current_time;
                self.transition(transport, r.request_id, |c| {
                    c.current_time = position;
                    c.idle_reason = None;
                })
                .await;
                self.renderer
                    .send(PlayerCommand::Seek {
                        session_id: transport.to_string(),
                        position,
                    })
                    .await;
                self.notify_app(transport).await;
            }
            MediaRequest::Stop(r) => {
                self.transition(transport, r.request_id, |c| {
                    c.set_idle(Some(IdleReason::Cancelled));
                })
                .await;
                self.renderer
                    .send(PlayerCommand::Stop {
                        session_id: transport.to_string(),
                    })
                    .await;
                if let Some(session) = self.sessions.get_mut(transport) {
                    session.coordinator.clear_media();
                }
                self.unregister_proxies(transport);
                self.notify_app(transport).await;
            }
            MediaRequest::SetVolume(r) => {
                let (level, muted) = match self.sessions.get_mut(transport) {
                    Some(session) => {
                        session.coordinator.volume.apply_update(&r.volume);
                        (
                            session.coordinator.volume.level,
                            session.coordinator.volume.muted,
                        )
                    }
                    None => return,
                };
                let response = match self.sessions.get(transport) {
                    Some(session) => session.coordinator.status_response(r.request_id),
                    None => return,
                };
                self.broadcast(transport, ns::MEDIA, &response).await;
                self.renderer
                    .send(PlayerCommand::Volume {
                        session_id: transport.to_string(),
                        level,
                        muted,
                    })
                    .await;
                self.notify_app(transport).await;
            }
            MediaRequest::GetStatus(r) => {
                if let Some(session) = self.sessions.get(transport) {
                    let response = session.coordinator.status_response(r.request_id);
                    self.send_to(conn_id, transport, source, ns::MEDIA, &response)
                        .await;
                }
            }
            MediaRequest::QueueGetItemIds(r) => {
                let item_ids = match self.sessions.get(transport) {
                    Some(session)
                        if session.coordinator.current_media.is_some()
                            && session.coordinator.player_state != PlayerState::Idle =>
                    {
                        vec![session.coordinator.media_session_id]
                    }
                    _ => Vec::new(),
                };
                let response = QueueItemIdsResponse::new(r.request_id, item_ids);
                self.send_to(conn_id, transport, source, ns::MEDIA, &response)
                    .await;
            }
            MediaRequest::QueueLoad(r) => {
                let response =
                    vibecast_messages::MediaStatusResponse::new(r.request_id, Vec::new());
                self.send_to(conn_id, transport, source, ns::MEDIA, &response)
                    .await;
            }
        }
    }

    /// Apply a coordinator mutation and broadcast the resulting status. The
    /// caller then drives the renderer and notifies the app.
    async fn transition(
        &mut self,
        transport: &str,
        request_id: i64,
        mutate: impl FnOnce(&mut Coordinator),
    ) {
        let response = match self.sessions.get_mut(transport) {
            Some(session) => {
                mutate(&mut session.coordinator);
                session.coordinator.status_response(request_id)
            }
            None => return,
        };
        self.broadcast(transport, ns::MEDIA, &response).await;
    }

    async fn media_load(
        &mut self,
        conn_id: u64,
        transport: &str,
        source: &str,
        load: Box<LoadRequest>,
    ) {
        // Phase 1: broadcast IDLE + LOADING with the request's media info.
        let response = match self.sessions.get_mut(transport) {
            Some(session) => {
                let coordinator = &mut session.coordinator;
                coordinator.media_session_id += 1;
                let loading = loading_media_info(&load);
                coordinator.current_media = Some(loading.clone());
                coordinator.player_state = PlayerState::Idle;
                coordinator.idle_reason = None;
                coordinator.current_time = 0.0;
                coordinator.loading_response(load.request_id, &loading)
            }
            None => return,
        };
        self.broadcast(transport, ns::MEDIA, &response).await;

        // Phase 2: resolve media off the mailbox and feed the result back.
        let (app, ctx) = match self.sessions.get(transport) {
            Some(session) => (session.app.clone(), session.ctx.clone()),
            None => return,
        };
        let self_tx = self.self_tx.clone();
        let session_id = transport.to_string();
        let sender_id = source.to_string();
        let request_id = load.request_id;
        tokio::spawn(async move {
            let result = app.resolve_media(&ctx, &load).await;
            let _ = self_tx
                .send(HubEvent::MediaResolved(MediaResolved {
                    session_id,
                    request_id,
                    connection_id: conn_id,
                    sender_id,
                    result,
                }))
                .await;
        });
    }

    async fn on_media_resolved(&mut self, resolved: MediaResolved) {
        let MediaResolved {
            session_id,
            request_id,
            connection_id,
            sender_id,
            result,
        } = resolved;
        if !self.sessions.contains_key(&session_id) {
            return; // session stopped while resolving
        }

        let mut media = match result {
            Ok(media) => media,
            Err(failure) => {
                self.fail_load(&session_id, connection_id, &sender_id, request_id, &failure)
                    .await;
                return;
            }
        };
        media.session_id = session_id.clone();
        if media.streams.is_empty() {
            let failure = MediaResolveError::internal("INVALID_APP_MEDIA");
            self.fail_load(&session_id, connection_id, &sender_id, request_id, &failure)
                .await;
            return;
        }

        let app_key = self
            .sessions
            .get(&session_id)
            .map(|s| s.app_key.clone())
            .unwrap_or_default();
        let media = self.attach_proxies(&session_id, &app_key, media);

        // Phase 3 + 4: broadcast resolved LOADING, then BUFFERING, then load.
        let (loading, buffering) = match self.sessions.get_mut(&session_id) {
            Some(session) => {
                let coordinator = &mut session.coordinator;
                let info = media_info(&media);
                coordinator.playback_media = Some(media.clone());
                coordinator.current_media = Some(info.clone());
                coordinator.current_time = media.start_time;
                let loading = coordinator.loading_response(request_id, &info);
                coordinator.player_state = PlayerState::Buffering;
                coordinator.idle_reason = None;
                let buffering = coordinator.status_response(request_id);
                (loading, buffering)
            }
            None => return,
        };
        self.broadcast(&session_id, ns::MEDIA, &loading).await;
        self.broadcast(&session_id, ns::MEDIA, &buffering).await;

        self.renderer
            .send(PlayerCommand::Load {
                session_id: session_id.clone(),
                media: to_payload(&media),
            })
            .await;
        self.notify_app(&session_id).await;
    }

    fn attach_proxies(
        &self,
        session_id: &str,
        app_key: &str,
        mut media: PlaybackMedia,
    ) -> PlaybackMedia {
        let (manifest_routes, license_routes) = collect_routes(&media);
        let has_manifest = !manifest_routes.is_empty();
        let has_license = !license_routes.is_empty();
        if !has_manifest && !has_license {
            return media;
        }
        let Some(session) = self.sessions.get(session_id) else {
            return media;
        };
        let proxy = Arc::new(SessionProxy::new(
            app_key.to_string(),
            session.app.clone(),
            self.callback_context(session, None),
            manifest_routes,
            license_routes,
        ));
        let manifest_base =
            has_manifest.then(|| self.proxy.register_manifest(session_id, proxy.clone()));
        let license_base =
            has_license.then(|| self.proxy.register_license(session_id, proxy.clone()));
        rewrite_streams(
            &mut media,
            manifest_base.as_deref(),
            license_base.as_deref(),
        );
        media
    }

    async fn fail_load(
        &mut self,
        session_id: &str,
        conn_id: u64,
        sender_id: &str,
        request_id: i64,
        failure: &MediaResolveError,
    ) {
        tracing::warn!(
            session = %session_id,
            reason = %failure.reason(),
            detail = ?failure.detail_code,
            retryable = failure.retryable,
            "load failed"
        );
        let (failed, status) = match self.sessions.get_mut(session_id) {
            Some(session) => {
                let coordinator = &mut session.coordinator;
                coordinator.set_idle(Some(IdleReason::Error));
                coordinator.clear_media();
                (
                    LoadFailedResponse::new(request_id, failure.reason()),
                    coordinator.status_response(request_id),
                )
            }
            None => return,
        };
        self.unregister_proxies(session_id);
        self.send_to(conn_id, session_id, sender_id, ns::MEDIA, &failed)
            .await;
        self.broadcast(session_id, ns::MEDIA, &status).await;
        self.notify_app(session_id).await;
    }

    // -- renderer reports ---------------------------------------------------

    async fn on_report(&mut self, report: PlayerReport) {
        let session_id = report.session_id().to_string();
        if !self.sessions.contains_key(&session_id) {
            return;
        }
        match report {
            PlayerReport::State {
                player_state,
                current_time,
                duration,
                idle_reason,
                ..
            } => {
                self.apply_state(
                    &session_id,
                    player_state,
                    current_time,
                    duration,
                    idle_reason,
                )
                .await;
            }
            PlayerReport::Error { code, message, .. } => {
                tracing::warn!(session = %session_id, %code, %message, "player error");
                let (current_time, duration) = match self.sessions.get(&session_id) {
                    Some(session) => (
                        session.coordinator.current_time,
                        session
                            .coordinator
                            .current_media
                            .as_ref()
                            .and_then(|m| m.duration),
                    ),
                    None => return,
                };
                self.apply_state(
                    &session_id,
                    PlayerState::Idle,
                    current_time,
                    duration,
                    Some(IdleReason::Error),
                )
                .await;
            }
        }
    }

    async fn apply_state(
        &mut self,
        session_id: &str,
        player_state: PlayerState,
        current_time: f64,
        duration: Option<f64>,
        idle_reason: Option<IdleReason>,
    ) {
        let response = match self.sessions.get_mut(session_id) {
            Some(session) => {
                let coordinator = &mut session.coordinator;
                coordinator.player_state = player_state;
                coordinator.current_time = current_time;
                coordinator.idle_reason = idle_reason;
                if let Some(duration) = duration {
                    if let Some(media) = &mut coordinator.current_media {
                        media.duration = Some(duration);
                    }
                    if let Some(media) = &mut coordinator.playback_media {
                        media.duration = Some(duration);
                    }
                }
                coordinator.status_response(0)
            }
            None => return,
        };
        if player_state == PlayerState::Idle {
            self.unregister_proxies(session_id);
        }
        self.broadcast(session_id, ns::MEDIA, &response).await;
        self.notify_app(session_id).await;
    }

    // -- helpers ------------------------------------------------------------

    /// Current connection handles subscribed to a transport (for broadcasts).
    fn subscriber_handles(&self, transport: &str) -> Vec<ConnectionHandle> {
        let connection_ids: HashSet<u64> = self
            .subscriptions
            .iter()
            .filter(|(_, target)| target.as_str() == transport)
            .map(|((conn, _), _)| *conn)
            .collect();
        connection_ids
            .iter()
            .filter_map(|id| self.connections.get(id).cloned())
            .collect()
    }

    /// Build a per-callback app context whose custom-message sender writes to
    /// the bound sender (if any) or broadcasts to the transport's subscribers.
    fn callback_context(&self, session: &Session, bound: Option<(u64, String)>) -> AppContext {
        let transport_id = session.ctx.transport_id.clone();
        let subscribers = self.subscriber_handles(&transport_id);
        let bound = bound.and_then(|(conn_id, sender_id)| {
            self.connections
                .get(&conn_id)
                .map(|handle| (handle.clone(), sender_id))
        });
        let sender = Arc::new(HubSender {
            transport_id: transport_id.clone(),
            bound,
            subscribers,
        });
        AppContext::new(
            session.ctx.session_id.clone(),
            transport_id,
            session.ctx.app_id.clone(),
            session.ctx.http.clone(),
            session.ctx.receiver.clone(),
            sender,
        )
    }

    async fn notify_app(&self, session_id: &str) {
        let (app, ctx, state) = match self.sessions.get(session_id) {
            Some(session) => {
                let coordinator = &session.coordinator;
                let state = PlaybackState {
                    player_state: coordinator.player_state,
                    current_time: coordinator.current_time,
                    duration: coordinator.current_media.as_ref().and_then(|m| m.duration),
                    idle_reason: coordinator.idle_reason,
                };
                (
                    session.app.clone(),
                    self.callback_context(session, None),
                    state,
                )
            }
            None => return,
        };
        app.on_playback_update(&ctx, state).await;
    }

    fn unregister_proxies(&self, session_id: &str) {
        self.proxy.unregister_manifest(session_id);
        self.proxy.unregister_license(session_id);
    }

    async fn send_to<T: Serialize>(
        &self,
        conn_id: u64,
        source: &str,
        dest: &str,
        namespace: &str,
        message: &T,
    ) {
        let Some(handle) = self.connections.get(&conn_id) else {
            return;
        };
        let Ok(value) = serde_json::to_value(message) else {
            return;
        };
        let _ = handle.send_json(source, dest, namespace, &value).await;
    }

    async fn broadcast<T: Serialize>(&self, transport: &str, namespace: &str, message: &T) {
        let Ok(value) = serde_json::to_value(message) else {
            return;
        };
        let connection_ids: HashSet<u64> = self
            .subscriptions
            .iter()
            .filter(|(_, target)| target.as_str() == transport)
            .map(|((conn, _), _)| *conn)
            .collect();
        for conn_id in connection_ids {
            if let Some(handle) = self.connections.get(&conn_id) {
                let _ = handle.send_json(transport, "*", namespace, &value).await;
            }
        }
    }
}

fn parse_payload(message: &CastMessage) -> Option<serde_json::Value> {
    let text = message.payload_utf8.as_deref()?;
    serde_json::from_str(text).ok()
}
