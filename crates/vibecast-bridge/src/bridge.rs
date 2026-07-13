//! The player bridge: an axum HTTP/WebSocket server where external players
//! (browser / Kodi) register, receive their command stream, and report playback
//! state — plus DRM license and manifest proxy routes.
//!
//! Each `/player` WebSocket connection is an independent player: its first frame
//! is a `register` message carrying the player's id, name, and capabilities. The
//! bridge emits a [`PlayerEvent::Registered`] carrying a per-player command sink
//! ([`Player`]) and a per-player [`PlayerReport`] stream, so the orchestrator can
//! give that player its own Cast receiver. There is no cross-player fan-out: a
//! command sent to one player's sink reaches only that player's socket.
//!
//! The license/manifest handler registries are plain maps keyed by session id
//! (session ids are globally unique), shared across all players' receivers.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU16, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::body::{Body, Bytes};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use vibecast_player_api::headers::{filter_upstream_headers, filter_upstream_response_headers};
use vibecast_player_api::{
    AppSettingsPayload, ClientMessage, LicenseHandler, LicenseRequest, ManifestHandler,
    ManifestProxyRequest, Player, PlayerCommand, PlayerRegistration, PlayerReport, ProxyRegistrar,
    RouteId, ServerMessage, SettingOptionPayload, SettingPayload, SettingsSnapshotMessage,
    SettingsUpdateResultMessage, SettingsUpdateStatus,
};
use vibecast_settings::{
    AppSettingsReader, AppSettingsSchema, PlayerSettings, SettingDescriptor, SettingMutation,
    SettingValue, SettingsService, SettingsServiceError, SettingsSnapshot,
};

use crate::web::{PLAYER_HTML, PLAYER_HTML_CONTENT_TYPE, PLAYER_JS, PLAYER_JS_CONTENT_TYPE};

/// Lifecycle events emitted by the bridge as players connect and disconnect.
///
/// Both variants carry an `epoch`: a per-connection token, unique across the
/// bridge's lifetime, that distinguishes two sockets sharing the same
/// `player_id` (two browser tabs, or an overlapping reconnect). The orchestrator
/// uses it so a stale connection's [`Disconnected`](Self::Disconnected) can't
/// tear down the newer receiver that replaced it.
pub enum PlayerEvent {
    /// A player registered. Carries its command sink and report stream so the
    /// orchestrator can bind it to a dedicated Cast receiver.
    Registered {
        /// Reported identity + capabilities (boxed: much larger than the other
        /// variant).
        registration: Box<PlayerRegistration>,
        /// Command sink for this player (routes only to its socket).
        player: Arc<dyn Player>,
        /// Playback reports from this player.
        reports: mpsc::Receiver<PlayerReport>,
        /// Settings scoped to this player's stable id.
        settings: PlayerSettings,
        /// Per-connection token identifying the socket this registration came
        /// from.
        epoch: u64,
    },
    /// The player with this id disconnected; its receiver should be torn down.
    Disconnected {
        /// The player id from its registration.
        player_id: String,
        /// The epoch of the socket that disconnected (matches its
        /// [`Registered`](Self::Registered) epoch).
        epoch: u64,
    },
}

/// A per-player command sink: serializes commands to one socket's out channel.
struct PlayerSink {
    out: mpsc::Sender<String>,
}

#[async_trait]
impl Player for PlayerSink {
    async fn send(&self, command: PlayerCommand) {
        match serde_json::to_string(&ServerMessage::Playback(command)) {
            Ok(text) => {
                let _ = self.out.send(text).await;
            }
            Err(error) => tracing::error!(%error, "failed to serialize player command"),
        }
    }
}

// -- bridge server ---------------------------------------------------------

/// Shared, cheaply-cloneable bridge state (axum handler state).
#[derive(Clone)]
struct BridgeState {
    events: mpsc::Sender<PlayerEvent>,
    settings: SettingsService,
    licenses: Arc<Mutex<HashMap<String, Arc<dyn LicenseHandler>>>>,
    manifests: Arc<Mutex<HashMap<String, Arc<dyn ManifestHandler>>>>,
    resolved_host: Arc<str>,
    configured_port: u16,
    port: Arc<AtomicU16>,
    /// Monotonic counter handing each socket a unique per-connection epoch.
    epochs: Arc<AtomicU64>,
    /// Current writable connection epoch for each stable player id.
    current_epochs: Arc<Mutex<HashMap<String, u64>>>,
}

impl BridgeState {
    fn license_handler(&self, session_id: &str) -> Option<Arc<dyn LicenseHandler>> {
        self.licenses.lock().unwrap().get(session_id).cloned()
    }

    fn manifest_handler(&self, session_id: &str) -> Option<Arc<dyn ManifestHandler>> {
        self.manifests.lock().unwrap().get(session_id).cloned()
    }

    fn serving_port(&self) -> Option<u16> {
        let port = self.port.load(Ordering::SeqCst);
        (port != 0).then_some(port)
    }

    fn effective_port(&self) -> u16 {
        self.serving_port().unwrap_or(self.configured_port)
    }
}

struct RunningTasks {
    server: JoinHandle<()>,
    shutdown: oneshot::Sender<()>,
}

/// HTTP/WebSocket bridge where external players register and receive commands.
pub struct PlayerBridge {
    state: BridgeState,
    bind_host: Arc<str>,
    tasks: Mutex<Option<RunningTasks>>,
}

impl PlayerBridge {
    /// Create a bridge. Construction is side-effect free: no tasks are spawned
    /// and no runtime is required until [`start`](Self::start). Player lifecycle
    /// events are delivered on `events`, and `settings` supplies each registered
    /// player with its scoped app settings.
    #[must_use]
    pub fn new(
        host: impl Into<String>,
        port: u16,
        events: mpsc::Sender<PlayerEvent>,
        settings: SettingsService,
    ) -> Self {
        let host = host.into();
        let resolved_host = if host == "0.0.0.0" || host == "::" {
            "127.0.0.1".to_string()
        } else {
            host.clone()
        };

        let state = BridgeState {
            events,
            settings,
            licenses: Arc::new(Mutex::new(HashMap::new())),
            manifests: Arc::new(Mutex::new(HashMap::new())),
            resolved_host: Arc::from(resolved_host.as_str()),
            configured_port: port,
            port: Arc::new(AtomicU16::new(0)),
            epochs: Arc::new(AtomicU64::new(0)),
            current_epochs: Arc::new(Mutex::new(HashMap::new())),
        };

        Self {
            state,
            bind_host: Arc::from(host.as_str()),
            tasks: Mutex::new(None),
        }
    }

    /// Bind the listener and start serving. A second call while already running
    /// is a no-op.
    pub async fn start(&self) -> std::io::Result<()> {
        if self.tasks.lock().unwrap().is_some() {
            return Ok(());
        }

        let listener =
            tokio::net::TcpListener::bind((self.bind_host.as_ref(), self.state.configured_port))
                .await?;
        let port = listener.local_addr()?.port();
        self.state.port.store(port, Ordering::SeqCst);

        let app = router(self.state.clone());
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let server_handle = tokio::spawn(async move {
            let served = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                })
                .await;
            if let Err(error) = served {
                tracing::error!(%error, "player bridge server stopped");
            }
        });

        *self.tasks.lock().unwrap() = Some(RunningTasks {
            server: server_handle,
            shutdown: shutdown_tx,
        });

        tracing::info!(
            host = %self.state.resolved_host,
            port,
            "player bridge started (web=http://{}:{}/)",
            self.state.resolved_host,
            port
        );
        Ok(())
    }

    /// Gracefully stop serving, await task completion, and clear session
    /// handlers. Safe to call when not running.
    pub async fn stop(&self) {
        let tasks = self.tasks.lock().unwrap().take();
        if let Some(tasks) = tasks {
            let _ = tasks.shutdown.send(());
            if let Err(error) = tasks.server.await {
                tracing::warn!(%error, "player bridge server task join failed");
            }
        }
        self.state.port.store(0, Ordering::SeqCst);
        self.state.licenses.lock().unwrap().clear();
        self.state.manifests.lock().unwrap().clear();
        tracing::info!("player bridge stopped");
    }

    /// The bound TCP port (available after [`start`](Self::start)).
    #[must_use]
    pub fn serving_port(&self) -> Option<u16> {
        self.state.serving_port()
    }

    /// Register a session license handler; returns its proxy URL.
    pub fn register_license_handler(
        &self,
        session_id: impl Into<String>,
        handler: Arc<dyn LicenseHandler>,
    ) -> String {
        let session_id = session_id.into();
        self.state
            .licenses
            .lock()
            .unwrap()
            .insert(session_id.clone(), handler);
        format!(
            "http://{}:{}/license/{}",
            self.state.resolved_host,
            self.state.effective_port(),
            session_id
        )
    }

    /// Unregister a session license handler.
    pub fn unregister_license_handler(&self, session_id: &str) {
        self.state.licenses.lock().unwrap().remove(session_id);
    }

    /// Register a session manifest handler; returns its proxy URL prefix.
    pub fn register_manifest_handler(
        &self,
        session_id: impl Into<String>,
        handler: Arc<dyn ManifestHandler>,
    ) -> String {
        let session_id = session_id.into();
        self.state
            .manifests
            .lock()
            .unwrap()
            .insert(session_id.clone(), handler);
        format!(
            "http://{}:{}/manifest/{}",
            self.state.resolved_host,
            self.state.effective_port(),
            session_id
        )
    }

    /// Unregister a session manifest handler.
    pub fn unregister_manifest_handler(&self, session_id: &str) {
        self.state.manifests.lock().unwrap().remove(session_id);
    }
}

impl ProxyRegistrar for PlayerBridge {
    fn register_license(&self, session_id: &str, handler: Arc<dyn LicenseHandler>) -> String {
        self.register_license_handler(session_id.to_string(), handler)
    }

    fn unregister_license(&self, session_id: &str) {
        self.unregister_license_handler(session_id);
    }

    fn register_manifest(&self, session_id: &str, handler: Arc<dyn ManifestHandler>) -> String {
        self.register_manifest_handler(session_id.to_string(), handler)
    }

    fn unregister_manifest(&self, session_id: &str) {
        self.unregister_manifest_handler(session_id);
    }
}

fn router(state: BridgeState) -> Router {
    Router::new()
        .route("/", get(serve_page))
        .route("/index.html", get(serve_page))
        .route("/player.js", get(serve_script))
        .route("/player", get(ws_handler))
        .route("/license/{session_id}", post(license_handler))
        .route(
            "/manifest/{session_id}/{route_path}",
            get(manifest_handler).head(manifest_handler),
        )
        .with_state(state)
}

async fn serve_page() -> impl IntoResponse {
    ([(CONTENT_TYPE, PLAYER_HTML_CONTENT_TYPE)], PLAYER_HTML)
}

async fn serve_script() -> impl IntoResponse {
    ([(CONTENT_TYPE, PLAYER_JS_CONTENT_TYPE)], PLAYER_JS)
}

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<BridgeState>) -> Response {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn settings_snapshot(
    service: &SettingsService,
    settings: &PlayerSettings,
) -> Result<SettingsSnapshotMessage, SettingsServiceError> {
    let mut apps = Vec::new();
    for schema in service.catalog().apps() {
        let snapshot = settings.reader(schema.app_id()).await?.snapshot();
        apps.push(app_settings_payload(schema, &snapshot));
    }
    Ok(SettingsSnapshotMessage::new(apps))
}

fn app_settings_payload(
    schema: &AppSettingsSchema,
    snapshot: &SettingsSnapshot,
) -> AppSettingsPayload {
    let settings = schema
        .settings()
        .iter()
        .map(|descriptor| {
            let (kind, options) = match descriptor {
                SettingDescriptor::Boolean { .. } => ("boolean", Vec::new()),
                SettingDescriptor::Choice { choices, .. } => (
                    "choice",
                    choices
                        .iter()
                        .map(|choice| SettingOptionPayload {
                            value: choice.value.clone(),
                            label: choice.label.clone(),
                        })
                        .collect(),
                ),
                SettingDescriptor::Integer { .. } => ("integer", Vec::new()),
                SettingDescriptor::Number { .. } => ("number", Vec::new()),
                SettingDescriptor::String { .. } => ("string", Vec::new()),
            };
            SettingPayload {
                key: descriptor.key().to_owned(),
                label: descriptor.label().to_owned(),
                description: descriptor.description().map(str::to_owned),
                kind: kind.to_owned(),
                default: descriptor.default_value(),
                value: snapshot
                    .values()
                    .get(descriptor.key())
                    .cloned()
                    .unwrap_or_else(|| descriptor.default_value()),
                writable: descriptor.scope() == vibecast_settings::SettingScope::AppPlayer,
                options,
            }
        })
        .collect();

    AppSettingsPayload {
        app_key: schema.app_id().to_owned(),
        display_name: schema.display_name().to_owned(),
        revision: snapshot.revision(),
        settings,
    }
}

async fn current_app_settings_payload(
    service: &SettingsService,
    settings: &PlayerSettings,
    app_key: &str,
) -> Option<AppSettingsPayload> {
    let schema = service.catalog().app(app_key)?;
    let snapshot = settings.reader(app_key).await.ok()?.snapshot();
    Some(app_settings_payload(schema, &snapshot))
}

async fn apply_settings_update(
    service: &SettingsService,
    settings: &PlayerSettings,
    request_id: String,
    app_key: String,
    expected_revision: u64,
    changes: std::collections::BTreeMap<String, Option<vibecast_settings::SettingValue>>,
) -> ServerMessage {
    let schema = service.catalog().app(&app_key);
    let mutations = changes
        .into_iter()
        .map(|(key, value)| match value {
            Some(value) => {
                let value = match (schema.and_then(|schema| schema.setting(&key)), value) {
                    (Some(SettingDescriptor::Number { .. }), SettingValue::Integer(value)) => {
                        SettingValue::Number(value as f64)
                    }
                    (_, value) => value,
                };
                SettingMutation::Set { key, value }
            }
            None => SettingMutation::Reset { key },
        })
        .collect();

    let result = settings
        .compare_and_set(&app_key, expected_revision, mutations)
        .await;
    let (status, app) = match result {
        Ok(snapshot) => {
            let app = service
                .catalog()
                .app(&app_key)
                .map(|schema| app_settings_payload(schema, &snapshot));
            (SettingsUpdateStatus::Applied, app)
        }
        Err(SettingsServiceError::Conflict { .. }) => (
            SettingsUpdateStatus::Conflict,
            current_app_settings_payload(service, settings, &app_key).await,
        ),
        Err(_) => {
            tracing::warn!(app_key, "player settings update rejected");
            (
                SettingsUpdateStatus::Rejected,
                current_app_settings_payload(service, settings, &app_key).await,
            )
        }
    };
    let app = app.unwrap_or_else(|| AppSettingsPayload {
        app_key: app_key.clone(),
        display_name: app_key,
        revision: 0,
        settings: Vec::new(),
    });

    ServerMessage::SettingsUpdateResult(SettingsUpdateResultMessage::new(request_id, status, app))
}

async fn handle_socket(socket: WebSocket, state: BridgeState) {
    let (mut sink, mut stream) = socket.split();

    // The first frame must be a `register` client message.
    let Some(Ok(Message::Text(first_frame))) = stream.next().await else {
        return;
    };
    let Ok(ClientMessage::Register {
        player: mut registration,
    }) = serde_json::from_str::<ClientMessage>(first_frame.as_str())
    else {
        tracing::warn!("first player frame was not a valid register message");
        return;
    };

    registration.player_id = registration.player_id.trim().to_owned();
    registration.name = registration.name.trim().to_owned();
    if registration.player_id.is_empty() || registration.name.is_empty() {
        tracing::warn!("player registration has an empty id or name");
        return;
    }

    let player_id = registration.player_id.clone();
    let epoch = state.epochs.fetch_add(1, Ordering::Relaxed);
    let player_settings = match state.settings.player(player_id.clone()) {
        Ok(settings) => settings,
        Err(_) => return,
    };
    let mut settings_readers = Vec::new();
    for schema in state.settings.catalog().apps() {
        match player_settings.reader(schema.app_id()).await {
            Ok(reader) => settings_readers.push(reader),
            Err(_) => {
                tracing::warn!(player_id, "failed to subscribe to player settings");
                return;
            }
        }
    }
    let snapshot = match settings_snapshot(&state.settings, &player_settings).await {
        Ok(snapshot) => snapshot,
        Err(_) => {
            tracing::warn!(player_id, "failed to build player settings snapshot");
            return;
        }
    };
    let mut last_settings_revisions = snapshot
        .apps
        .iter()
        .map(|app| (app.app_key.clone(), app.revision))
        .collect::<BTreeMap<_, _>>();
    let snapshot = ServerMessage::SettingsSnapshot(snapshot);
    let snapshot = match serde_json::to_string(&snapshot) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            tracing::error!(%error, "failed to serialize player settings snapshot");
            return;
        }
    };
    if sink.send(Message::Text(snapshot.into())).await.is_err() {
        return;
    }

    let (settings_changed_tx, mut settings_changed_rx) = mpsc::channel::<()>(1);
    let mut settings_watch_tasks = Vec::new();
    for reader in settings_readers {
        settings_watch_tasks.push(spawn_settings_watch(reader, settings_changed_tx.clone()));
    }
    drop(settings_changed_tx);

    let (out_tx, mut out_rx) = mpsc::channel::<String>(64);
    let (reports_tx, reports_rx) = mpsc::channel::<PlayerReport>(64);
    let player: Arc<dyn Player> = Arc::new(PlayerSink { out: out_tx });
    let previous_epoch = state
        .current_epochs
        .lock()
        .unwrap()
        .insert(player_id.clone(), epoch);

    if state
        .events
        .send(PlayerEvent::Registered {
            registration: Box::new(registration),
            player,
            reports: reports_rx,
            settings: player_settings.clone(),
            epoch,
        })
        .await
        .is_err()
    {
        let mut current_epochs = state.current_epochs.lock().unwrap();
        if current_epochs.get(&player_id).copied() == Some(epoch) {
            match previous_epoch {
                Some(previous_epoch) => {
                    current_epochs.insert(player_id, previous_epoch);
                }
                None => {
                    current_epochs.remove(&player_id);
                }
            }
        }
        return;
    }
    tracing::info!(player_id = %player_id, "player registered");

    loop {
        tokio::select! {
            outgoing = out_rx.recv() => {
                match outgoing {
                    Some(text) => {
                        if sink.send(Message::Text(text.into())).await.is_err() {
                            break;
                        }
                    }
                    None => break,
                }
            }
            changed = settings_changed_rx.recv() => {
                if changed.is_none() {
                    break;
                }
                let snapshot = match settings_snapshot(&state.settings, &player_settings).await {
                    Ok(snapshot) => snapshot,
                    Err(error) => {
                        tracing::warn!(%error, player_id, "failed to refresh player settings snapshot");
                        continue;
                    }
                };
                if snapshot.apps.iter().all(|app| {
                    last_settings_revisions.get(&app.app_key).copied() == Some(app.revision)
                }) {
                    continue;
                }
                last_settings_revisions = snapshot
                    .apps
                    .iter()
                    .map(|app| (app.app_key.clone(), app.revision))
                    .collect();
                let snapshot = ServerMessage::SettingsSnapshot(snapshot);
                match serde_json::to_string(&snapshot) {
                    Ok(text) => {
                        if sink.send(Message::Text(text.into())).await.is_err() {
                            break;
                        }
                    }
                    Err(error) => tracing::error!(%error, "failed to serialize player settings snapshot"),
                }
            }
            incoming = stream.next() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<ClientMessage>(text.as_str()) {
                            Ok(message @ ClientMessage::State { .. })
                            | Ok(message @ ClientMessage::Error { .. }) => {
                                if let Some(report) = message.into_report() {
                                    let _ = reports_tx.send(report).await;
                                }
                            }
                            Ok(ClientMessage::SettingsUpdate {
                                request_id,
                                app_key,
                                expected_revision,
                                changes,
                            }) => {
                                if state.current_epochs.lock().unwrap().get(&player_id).copied()
                                    != Some(epoch)
                                {
                                    tracing::warn!(player_id, "rejecting settings update from stale player connection");
                                    break;
                                }
                                let response = apply_settings_update(
                                    &state.settings,
                                    &player_settings,
                                    request_id,
                                    app_key,
                                    expected_revision,
                                    changes,
                                ).await;
                                if let ServerMessage::SettingsUpdateResult(result) = &response {
                                    last_settings_revisions
                                        .insert(result.app.app_key.clone(), result.app.revision);
                                }
                                match serde_json::to_string(&response) {
                                    Ok(text) => {
                                        if sink.send(Message::Text(text.into())).await.is_err() {
                                            break;
                                        }
                                    }
                                    Err(error) => {
                                        tracing::error!(%error, "failed to serialize settings update result");
                                    }
                                }
                            }
                            Ok(ClientMessage::Register { .. }) => {
                                tracing::warn!(player_id, "ignoring repeated player registration");
                            }
                            Err(_) => tracing::warn!("invalid player message payload"),
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(_)) => break,
                }
            }
        }
    }

    for task in settings_watch_tasks {
        task.abort();
    }

    let _ = state
        .events
        .send(PlayerEvent::Disconnected {
            player_id: player_id.clone(),
            epoch,
        })
        .await;
    let mut current_epochs = state.current_epochs.lock().unwrap();
    if current_epochs.get(&player_id).copied() == Some(epoch) {
        current_epochs.remove(&player_id);
    }
    tracing::info!(player_id = %player_id, "player disconnected");
}

fn spawn_settings_watch(
    mut reader: AppSettingsReader,
    changed: mpsc::Sender<()>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        while reader.changed().await.is_ok() {
            if changed.send(()).await.is_err() {
                break;
            }
        }
    })
}

async fn license_handler(
    Path(session_id): Path<String>,
    State(state): State<BridgeState>,
    Query(params): Query<HashMap<String, String>>,
    request_headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(handler) = state.license_handler(&session_id) else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let content_type = request_headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();
    let route_id = params.get("route").and_then(|raw| raw.parse().ok());
    let request = LicenseRequest {
        session_id,
        body: body.to_vec(),
        content_type,
        route_id,
        headers: filter_upstream_headers(&request_headers),
    };

    match handler.handle_license(request).await {
        Ok(response) => Response::builder()
            .status(response.status)
            .header(CONTENT_TYPE, response.content_type)
            .body(Body::from(response.body))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
        Err(error) => {
            tracing::warn!(%error, "license request failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

async fn manifest_handler(
    method: Method,
    Path((session_id, route_path)): Path<(String, String)>,
    State(state): State<BridgeState>,
    request_headers: HeaderMap,
) -> Response {
    let Some(handler) = state.manifest_handler(&session_id) else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let route_segment = route_path.split('.').next().unwrap_or("");
    let Ok(route_id) = route_segment.parse::<RouteId>() else {
        return StatusCode::BAD_REQUEST.into_response();
    };

    let is_head = method == Method::HEAD;
    let request = ManifestProxyRequest {
        session_id,
        route_id,
        method,
        headers: filter_upstream_headers(&request_headers),
    };

    match handler.handle_manifest(request).await {
        Ok(response) => {
            let mut builder = Response::builder()
                .status(response.status)
                .header(CONTENT_TYPE, response.content_type);
            let forwarded = filter_upstream_response_headers(&response.headers);
            for (name, value) in &forwarded {
                // Drop upstream caching directives; the proxy URL is reused
                // across stream switches (only the query version changes), so a
                // cached manifest would stall playback on the previous stream.
                if *name == http::header::CACHE_CONTROL
                    || *name == http::header::EXPIRES
                    || *name == http::header::ETAG
                {
                    continue;
                }
                builder = builder.header(name, value);
            }
            builder = builder.header("cache-control", "no-store, max-age=0");
            let body = if is_head { Vec::new() } else { response.body };
            builder
                .body(Body::from(body))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
        Err(error) => {
            tracing::warn!(%error, "manifest request failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::body::to_bytes;
    use axum::http::Request;
    use serde_json::{json, Value};
    use tokio_tungstenite::connect_async;
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    use super::*;
    use tokio::net::TcpStream;
    use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
    use vibecast_messages::{PlayerState, StreamType};
    use vibecast_player_api::{
        LicenseResponse, ManifestProxyResponse, PlaybackMediaPayload, PlaybackStreamPayload,
        ProxyError, ProxyResult,
    };
    use vibecast_settings::{
        AppSettingsSchema, ChoiceOption, MemorySettingsPersistence, SettingDescriptor,
        SettingScope, SettingValue, SettingsCatalog,
    };

    fn media() -> PlaybackMediaPayload {
        PlaybackMediaPayload {
            streams: vec![PlaybackStreamPayload {
                url: "https://example.com/manifest.mpd".into(),
                content_type: "application/dash+xml".into(),
                drm: None,
            }],
            stream_type: StreamType::Buffered,
            ..Default::default()
        }
    }

    fn settings_catalog() -> SettingsCatalog {
        SettingsCatalog::new(vec![AppSettingsSchema::with_display_name(
            "test-app",
            "Test App",
            vec![
                SettingDescriptor::Choice {
                    key: "quality".to_owned(),
                    label: "Quality".to_owned(),
                    description: Some("Preferred playback quality".to_owned()),
                    scope: SettingScope::AppPlayer,
                    default: "auto".to_owned(),
                    choices: vec![
                        ChoiceOption::new("auto", "Automatic"),
                        ChoiceOption::new("high", "High"),
                    ],
                },
                SettingDescriptor::Number {
                    key: "volume".to_owned(),
                    label: "Volume".to_owned(),
                    description: None,
                    scope: SettingScope::AppPlayer,
                    default: 0.5,
                    min: Some(0.0),
                    max: Some(1.0),
                },
            ],
        )
        .unwrap()])
        .unwrap()
    }

    async fn bridge() -> (PlayerBridge, mpsc::Receiver<PlayerEvent>) {
        let (events_tx, events_rx) = mpsc::channel(16);
        let settings = SettingsService::new(
            settings_catalog(),
            Arc::new(MemorySettingsPersistence::default()),
        )
        .await
        .unwrap();
        (
            PlayerBridge::new("127.0.0.1", 0, events_tx, settings),
            events_rx,
        )
    }

    async fn http_get(bridge: &PlayerBridge, uri: &str) -> (StatusCode, HeaderMap, Vec<u8>) {
        drive(
            bridge,
            Request::builder().uri(uri).body(Body::empty()).unwrap(),
        )
        .await
    }

    async fn drive(
        bridge: &PlayerBridge,
        request: Request<Body>,
    ) -> (StatusCode, HeaderMap, Vec<u8>) {
        use tower::ServiceExt;
        let response = router(bridge.state.clone()).oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec();
        (status, headers, body)
    }

    fn content_type(headers: &HeaderMap) -> &str {
        headers
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
    }

    #[tokio::test]
    async fn serves_default_shaka_player_page() {
        let (bridge, _events) = bridge().await;
        let (status, headers, body) = http_get(&bridge, "/").await;
        assert_eq!(status, StatusCode::OK);
        assert!(content_type(&headers).starts_with("text/html"));
        assert!(!body.is_empty());
    }

    #[tokio::test]
    async fn serves_player_script() {
        let (bridge, _events) = bridge().await;
        let (status, headers, body) = http_get(&bridge, "/player.js").await;
        assert_eq!(status, StatusCode::OK);
        assert!(content_type(&headers).contains("javascript"));
        assert!(!body.is_empty());
    }

    #[tokio::test]
    async fn start_and_stop() {
        let (bridge, _events) = bridge().await;
        bridge.start().await.expect("start");
        assert!(bridge.serving_port().is_some());
        bridge.stop().await;
        assert!(bridge.serving_port().is_none());
    }

    fn register_frame() -> String {
        json!({
            "type": "register",
            "player": {
                "playerId": "  player-1  ",
                "name": "  Test Player  ",
                "capabilities": {
                    "platform": "android",
                    "drm": [{ "system": "com.widevine.alpha", "securityLevel": "L1" }],
                    "videoCodecs": ["hevc", "h264"],
                    "maxResolution": { "width": 1920, "height": 1080 }
                }
            }
        })
        .to_string()
    }

    async fn connect(bridge: &PlayerBridge) -> WebSocketStream<MaybeTlsStream<TcpStream>> {
        let port = bridge.serving_port().expect("serving");
        let (ws, _) = connect_async(format!("ws://127.0.0.1:{port}/player"))
            .await
            .expect("ws connect");
        ws
    }

    async fn next_json<S>(ws: &mut S) -> Value
    where
        S: StreamExt<Item = Result<WsMessage, tokio_tungstenite::tungstenite::Error>> + Unpin,
    {
        loop {
            match tokio::time::timeout(Duration::from_secs(2), ws.next())
                .await
                .expect("frame within timeout")
            {
                Some(Ok(WsMessage::Text(text))) => {
                    return serde_json::from_str(text.as_str()).expect("json frame")
                }
                Some(Ok(_)) => continue,
                other => panic!("unexpected ws frame: {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn registration_delivers_capabilities_command_sink_and_reports() {
        let (bridge, mut events) = bridge().await;
        bridge.start().await.expect("start");
        let mut ws = connect(&bridge).await;

        // Register.
        ws.send(WsMessage::Text(register_frame().into()))
            .await
            .expect("send register");

        let snapshot = next_json(&mut ws).await;
        assert_eq!(snapshot["type"], "settingsSnapshot");

        // The bridge emits the protocol registration and a player-scoped settings handle.
        let event = tokio::time::timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("event within timeout")
            .expect("event");
        let (registration, player, mut reports, settings) = match event {
            PlayerEvent::Registered {
                registration,
                player,
                reports,
                settings,
                ..
            } => (registration, player, reports, settings),
            PlayerEvent::Disconnected { .. } => panic!("expected Registered"),
        };
        assert_eq!(registration.player_id, "player-1");
        assert_eq!(registration.name, "Test Player");
        assert_eq!(
            registration.capabilities.platform.as_deref(),
            Some("android")
        );
        assert_eq!(
            registration.capabilities.drm[0].system,
            "com.widevine.alpha"
        );
        assert_eq!(settings.player_id(), "player-1");

        // A command sent to this player's sink reaches its socket.
        player
            .send(PlayerCommand::Load {
                session_id: "s1".into(),
                media: media(),
            })
            .await;
        let command = next_json(&mut ws).await;
        assert_eq!(command["type"], "load");
        assert_eq!(command["sessionId"], "s1");
        assert!(command.get("playback").is_none());

        // Re-registering is ignored and does not replace this connection.
        ws.send(WsMessage::Text(register_frame_with_id("other").into()))
            .await
            .expect("send repeated register");

        // A report from the socket reaches this player's report stream.
        ws.send(WsMessage::Text(
            json!({
                "type": "state",
                "sessionId": "s1",
                "playerState": "PLAYING",
                "currentTime": 12.0
            })
            .to_string()
            .into(),
        ))
        .await
        .expect("send report");
        let report = tokio::time::timeout(Duration::from_secs(2), reports.recv())
            .await
            .expect("report within timeout")
            .expect("report");
        match report {
            PlayerReport::State {
                session_id,
                player_state,
                ..
            } => {
                assert_eq!(session_id, "s1");
                assert_eq!(player_state, PlayerState::Playing);
            }
            PlayerReport::Error { .. } => panic!("expected state report"),
        }

        // Closing the socket emits Disconnected.
        ws.close(None).await.ok();
        let event = tokio::time::timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("event within timeout")
            .expect("event");
        assert!(matches!(
            event,
            PlayerEvent::Disconnected { player_id, .. } if player_id == "player-1"
        ));

        bridge.stop().await;
    }

    #[tokio::test]
    async fn settings_snapshot_update_reset_rejection_and_conflict() {
        let (bridge, mut events) = bridge().await;
        bridge.start().await.expect("start");
        let mut ws = connect(&bridge).await;
        ws.send(WsMessage::Text(register_frame().into()))
            .await
            .expect("send register");

        let snapshot = next_json(&mut ws).await;
        assert_eq!(snapshot["type"], "settingsSnapshot");
        assert_eq!(snapshot["apps"][0]["appKey"], "test-app");
        assert_eq!(snapshot["apps"][0]["displayName"], "Test App");
        assert_eq!(snapshot["apps"][0]["revision"], 0);
        assert_eq!(snapshot["apps"][0]["settings"][0]["key"], "quality");
        assert_eq!(snapshot["apps"][0]["settings"][0]["kind"], "choice");
        assert_eq!(snapshot["apps"][0]["settings"][0]["default"], "auto");
        assert_eq!(snapshot["apps"][0]["settings"][0]["value"], "auto");
        assert_eq!(
            snapshot["apps"][0]["settings"][0]["options"],
            json!([
                { "value": "auto", "label": "Automatic" },
                { "value": "high", "label": "High" }
            ])
        );

        let (_player, _reports, settings) = match recv_event(&mut events).await {
            PlayerEvent::Registered {
                player,
                reports,
                settings,
                ..
            } => (player, reports, settings),
            PlayerEvent::Disconnected { .. } => panic!("expected Registered"),
        };
        assert_eq!(settings.player_id(), "player-1");

        ws.send(WsMessage::Text(
            json!({
                "type": "settingsUpdate",
                "requestId": "set",
                "appKey": "test-app",
                "expectedRevision": 0,
                "changes": { "quality": "high" }
            })
            .to_string()
            .into(),
        ))
        .await
        .expect("send update");
        let applied = next_json(&mut ws).await;
        assert_eq!(applied["type"], "settingsUpdateResult");
        assert_eq!(applied["requestId"], "set");
        assert_eq!(applied["status"], "applied");
        assert_eq!(applied["app"]["revision"], 1);
        assert_eq!(applied["app"]["settings"][0]["value"], "high");

        ws.send(WsMessage::Text(
            json!({
                "type": "settingsUpdate",
                "requestId": "reset",
                "appKey": "test-app",
                "expectedRevision": 1,
                "changes": { "quality": null }
            })
            .to_string()
            .into(),
        ))
        .await
        .expect("send reset");
        let reset = next_json(&mut ws).await;
        assert_eq!(reset["status"], "applied");
        assert_eq!(reset["app"]["revision"], 2);
        assert_eq!(reset["app"]["settings"][0]["value"], "auto");

        ws.send(WsMessage::Text(
            json!({
                "type": "settingsUpdate",
                "requestId": "conflict",
                "appKey": "test-app",
                "expectedRevision": 1,
                "changes": { "quality": "high" }
            })
            .to_string()
            .into(),
        ))
        .await
        .expect("send conflicting update");
        let conflict = next_json(&mut ws).await;
        assert_eq!(conflict["status"], "conflict");
        assert_eq!(conflict["app"]["revision"], 2);
        assert_eq!(conflict["app"]["settings"][0]["value"], "auto");

        ws.send(WsMessage::Text(
            json!({
                "type": "settingsUpdate",
                "requestId": "invalid",
                "appKey": "test-app",
                "expectedRevision": 2,
                "changes": { "quality": "invalid" }
            })
            .to_string()
            .into(),
        ))
        .await
        .expect("send invalid update");
        let rejected = next_json(&mut ws).await;
        assert_eq!(rejected["status"], "rejected");
        assert_eq!(rejected["app"]["revision"], 2);
        assert_eq!(rejected["app"]["settings"][0]["value"], "auto");

        drop(ws);
        bridge.stop().await;
    }

    #[tokio::test]
    async fn initial_snapshot_precedes_and_integral_number_update_is_applied() {
        use vibecast_settings::SettingKey;

        let (bridge, mut events) = bridge().await;
        bridge.start().await.expect("start");
        let mut ws = connect(&bridge).await;
        ws.send(WsMessage::Text(register_frame().into()))
            .await
            .expect("send register");
        ws.send(WsMessage::Text(
            json!({
                "type": "settingsUpdate",
                "requestId": "number",
                "appKey": "test-app",
                "expectedRevision": 0,
                "changes": { "volume": 1 }
            })
            .to_string()
            .into(),
        ))
        .await
        .expect("send update");

        let snapshot = next_json(&mut ws).await;
        assert_eq!(snapshot["type"], "settingsSnapshot");
        assert_eq!(snapshot["apps"][0]["revision"], 0);

        let settings = match recv_event(&mut events).await {
            PlayerEvent::Registered { settings, .. } => settings,
            PlayerEvent::Disconnected { .. } => panic!("expected Registered"),
        };
        let applied = next_json(&mut ws).await;
        assert_eq!(applied["type"], "settingsUpdateResult");
        assert_eq!(applied["requestId"], "number");
        assert_eq!(applied["status"], "applied");
        assert_eq!(applied["app"]["revision"], 1);

        let stored = settings.reader("test-app").await.unwrap().snapshot();
        assert_eq!(
            stored
                .get(SettingKey::<f64>::new("volume"))
                .expect("number type"),
            Some(1.0)
        );

        drop(ws);
        bridge.stop().await;
    }

    #[tokio::test]
    async fn external_settings_change_pushes_a_fresh_snapshot() {
        let (bridge, mut events) = bridge().await;
        bridge.start().await.expect("start");
        let mut ws = connect(&bridge).await;
        ws.send(WsMessage::Text(register_frame().into()))
            .await
            .expect("send register");
        let initial = next_json(&mut ws).await;
        assert_eq!(initial["apps"][0]["revision"], 0);

        let (_player, _reports, settings) = match recv_event(&mut events).await {
            PlayerEvent::Registered {
                player,
                reports,
                settings,
                ..
            } => (player, reports, settings),
            PlayerEvent::Disconnected { .. } => panic!("expected Registered"),
        };
        settings
            .compare_and_set(
                "test-app",
                0,
                vec![SettingMutation::Set {
                    key: "quality".to_owned(),
                    value: SettingValue::String("high".to_owned()),
                }],
            )
            .await
            .expect("external update");

        let pushed = next_json(&mut ws).await;
        assert_eq!(pushed["type"], "settingsSnapshot");
        assert_eq!(pushed["apps"][0]["revision"], 1);
        assert_eq!(pushed["apps"][0]["settings"][0]["value"], "high");

        drop(ws);
        bridge.stop().await;
    }

    #[tokio::test]
    async fn non_register_first_frame_is_rejected() {
        let (bridge, mut events) = bridge().await;
        bridge.start().await.expect("start");
        let mut ws = connect(&bridge).await;

        // A report before registering is invalid; the connection is dropped
        // without emitting any event.
        ws.send(WsMessage::Text(
            json!({ "type": "state", "sessionId": "s1", "playerState": "IDLE" })
                .to_string()
                .into(),
        ))
        .await
        .expect("send");

        let event = tokio::time::timeout(Duration::from_millis(300), events.recv()).await;
        assert!(event.is_err(), "no event should be emitted");
        bridge.stop().await;
    }

    #[tokio::test]
    async fn empty_registration_identity_is_rejected() {
        let (bridge, mut events) = bridge().await;
        bridge.start().await.expect("start");

        for player in [
            json!({ "playerId": "   ", "name": "Kodi" }),
            json!({ "playerId": "p1", "name": "   " }),
        ] {
            let mut ws = connect(&bridge).await;
            ws.send(WsMessage::Text(
                json!({ "type": "register", "player": player })
                    .to_string()
                    .into(),
            ))
            .await
            .expect("send invalid registration");
        }

        let event = tokio::time::timeout(Duration::from_millis(300), events.recv()).await;
        assert!(event.is_err(), "no event should be emitted");
        bridge.stop().await;
    }

    fn register_frame_with_id(player_id: &str) -> String {
        json!({
            "type": "register",
            "player": { "playerId": player_id, "name": "Dup Player" }
        })
        .to_string()
    }

    async fn recv_event(events: &mut mpsc::Receiver<PlayerEvent>) -> PlayerEvent {
        tokio::time::timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("event within timeout")
            .expect("event")
    }

    #[tokio::test]
    async fn duplicate_player_id_gets_distinct_epochs() {
        let (bridge, mut events) = bridge().await;
        bridge.start().await.expect("start");

        // Two sockets reuse the same player_id (e.g. two browser tabs, or an
        // overlapping reconnect).
        let mut ws1 = connect(&bridge).await;
        ws1.send(WsMessage::Text(register_frame_with_id("dup").into()))
            .await
            .expect("register ws1");
        // Retain the command sink + report stream so the socket stays open
        // (dropping the sink closes the socket, as a real orchestrator holds it).
        let (epoch1, _player1, _reports1) = match recv_event(&mut events).await {
            PlayerEvent::Registered {
                registration,
                player,
                reports,
                epoch,
                ..
            } => {
                assert_eq!(registration.player_id, "dup");
                (epoch, player, reports)
            }
            PlayerEvent::Disconnected { .. } => panic!("expected Registered"),
        };

        let mut ws2 = connect(&bridge).await;
        ws2.send(WsMessage::Text(register_frame_with_id("dup").into()))
            .await
            .expect("register ws2");
        let (epoch2, _player2, _reports2) = match recv_event(&mut events).await {
            PlayerEvent::Registered {
                player,
                reports,
                epoch,
                ..
            } => (epoch, player, reports),
            PlayerEvent::Disconnected { .. } => panic!("expected Registered"),
        };

        // Each connection is tagged with a distinct epoch.
        assert_ne!(epoch1, epoch2);

        // Consume both initial snapshots, then prove the older socket cannot
        // mutate the stable player's settings after the newer epoch took over.
        let _ = next_json(&mut ws1).await;
        let _ = next_json(&mut ws2).await;
        ws1.send(WsMessage::Text(
            json!({
                "type": "settingsUpdate",
                "requestId": "stale",
                "appKey": "test-app",
                "expectedRevision": 0,
                "changes": { "quality": "high" }
            })
            .to_string()
            .into(),
        ))
        .await
        .expect("send stale update");
        match recv_event(&mut events).await {
            PlayerEvent::Disconnected { player_id, epoch } => {
                assert_eq!(player_id, "dup");
                assert_eq!(epoch, epoch1);
            }
            PlayerEvent::Registered { .. } => panic!("expected Disconnected"),
        }
        let settings = bridge.state.settings.player("dup").unwrap();
        let reader = settings.reader("test-app").await.unwrap();
        assert_eq!(
            reader.snapshot().values()["quality"],
            SettingValue::String("auto".to_owned())
        );

        drop(ws2);
        bridge.stop().await;
    }

    struct EchoLicense;

    #[async_trait]
    impl LicenseHandler for EchoLicense {
        async fn handle_license(&self, request: LicenseRequest) -> ProxyResult<LicenseResponse> {
            Ok(LicenseResponse::ok(request.body))
        }
    }

    #[tokio::test]
    async fn license_proxy_round_trip() {
        let (bridge, _events) = bridge().await;
        bridge.register_license_handler("sess", Arc::new(EchoLicense));
        let request = Request::builder()
            .method(Method::POST)
            .uri("/license/sess")
            .header(CONTENT_TYPE, "application/octet-stream")
            .body(Body::from(vec![1u8, 2, 3]))
            .unwrap();
        let (status, _headers, body) = drive(&bridge, request).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, vec![1, 2, 3]);
    }

    #[tokio::test]
    async fn license_proxy_missing_handler_returns_404() {
        let (bridge, _events) = bridge().await;
        let request = Request::builder()
            .method(Method::POST)
            .uri("/license/unknown")
            .body(Body::from(vec![0u8]))
            .unwrap();
        let (status, _headers, _body) = drive(&bridge, request).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    struct ErrorLicense;

    #[async_trait]
    impl LicenseHandler for ErrorLicense {
        async fn handle_license(&self, _request: LicenseRequest) -> ProxyResult<LicenseResponse> {
            Ok(LicenseResponse {
                body: b"denied".to_vec(),
                content_type: "text/plain".into(),
                status: 403,
            })
        }
    }

    #[tokio::test]
    async fn license_proxy_preserves_explicit_error_response() {
        let (bridge, _events) = bridge().await;
        bridge.register_license_handler("sess", Arc::new(ErrorLicense));
        let request = Request::builder()
            .method(Method::POST)
            .uri("/license/sess")
            .body(Body::from(vec![0u8]))
            .unwrap();
        let (status, _headers, body) = drive(&bridge, request).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body, b"denied");
    }

    struct PanicLicense;

    #[async_trait]
    impl LicenseHandler for PanicLicense {
        async fn handle_license(&self, _request: LicenseRequest) -> ProxyResult<LicenseResponse> {
            Err(ProxyError::Internal("boom".into()))
        }
    }

    #[tokio::test]
    async fn license_proxy_unhandled_error_returns_500() {
        let (bridge, _events) = bridge().await;
        bridge.register_license_handler("sess", Arc::new(PanicLicense));
        let request = Request::builder()
            .method(Method::POST)
            .uri("/license/sess")
            .body(Body::from(vec![0u8]))
            .unwrap();
        let (status, _headers, _body) = drive(&bridge, request).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    }

    struct StaticManifest;

    #[async_trait]
    impl ManifestHandler for StaticManifest {
        async fn handle_manifest(
            &self,
            request: ManifestProxyRequest,
        ) -> ProxyResult<ManifestProxyResponse> {
            Ok(ManifestProxyResponse {
                body: format!("manifest for {}", request.route_id).into_bytes(),
                content_type: "application/dash+xml".into(),
                status: 200,
                headers: HeaderMap::new(),
            })
        }
    }

    #[tokio::test]
    async fn manifest_proxy_round_trip() {
        let (bridge, _events) = bridge().await;
        bridge.register_manifest_handler("sess", Arc::new(StaticManifest));
        let (status, headers, body) = http_get(&bridge, "/manifest/sess/m0.mpd").await;
        assert_eq!(status, StatusCode::OK);
        assert!(content_type(&headers).contains("dash"));
        assert_eq!(body, b"manifest for m0");
    }

    #[tokio::test]
    async fn manifest_proxy_head_request() {
        let (bridge, _events) = bridge().await;
        bridge.register_manifest_handler("sess", Arc::new(StaticManifest));
        let request = Request::builder()
            .method(Method::HEAD)
            .uri("/manifest/sess/m0.mpd")
            .body(Body::empty())
            .unwrap();
        let (status, _headers, body) = drive(&bridge, request).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.is_empty());
    }

    struct HeaderManifest;

    #[async_trait]
    impl ManifestHandler for HeaderManifest {
        async fn handle_manifest(
            &self,
            _request: ManifestProxyRequest,
        ) -> ProxyResult<ManifestProxyResponse> {
            let mut headers = HeaderMap::new();
            headers.insert("connection", "keep-alive".parse().unwrap());
            headers.insert("x-preserved", "yes".parse().unwrap());
            Ok(ManifestProxyResponse {
                body: b"ok".to_vec(),
                content_type: "application/dash+xml".into(),
                status: 200,
                headers,
            })
        }
    }

    #[tokio::test]
    async fn manifest_proxy_filters_hop_by_hop_response_headers() {
        let (bridge, _events) = bridge().await;
        bridge.register_manifest_handler("sess", Arc::new(HeaderManifest));
        let (status, headers, _body) = http_get(&bridge, "/manifest/sess/m0.mpd").await;
        assert_eq!(status, StatusCode::OK);
        assert!(!headers.contains_key("connection"));
        assert_eq!(
            headers.get("x-preserved").and_then(|v| v.to_str().ok()),
            Some("yes")
        );
    }
}
