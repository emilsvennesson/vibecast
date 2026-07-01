//! The player bridge: an axum HTTP/WebSocket server relaying player commands
//! to external renderers (browser / Kodi) and proxying DRM license and
//! manifest requests.
//!
//! Ports `vibecast._playback.player_bridge`. The connection registry,
//! primary/observer election and per-session resync snapshots live in a single
//! actor task (fed over an `mpsc`), preserving the serialized semantics of the
//! Python asyncio design without shared-mutable locking on the WebSocket path.
//! The license/manifest handler registries are plain maps (registered rarely,
//! looked up per request, then invoked outside any lock).

use std::collections::HashMap;
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
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use vibecast_messages::{IdleReason, PlayerState};

use crate::headers::{filter_upstream_headers, filter_upstream_response_headers};
use crate::protocol::{PlaybackMediaPayload, PlayerCommand, PlayerReport};
use crate::proxy::{LicenseHandler, LicenseRequest, ManifestHandler, ManifestProxyRequest};
use crate::web::{PLAYER_HTML, PLAYER_HTML_CONTENT_TYPE, PLAYER_JS, PLAYER_JS_CONTENT_TYPE};

/// A renderer that plays media in response to bridge commands.
///
/// The browser bridge is the default implementation; a future native renderer
/// can implement this trait without touching the coordinator.
#[async_trait]
pub trait Renderer: Send + Sync {
    /// Deliver a command to the renderer(s).
    async fn send(&self, command: PlayerCommand);
}

/// Requested role for a renderer WebSocket connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Auto,
    Primary,
    Observer,
}

impl Role {
    fn parse(raw: Option<&String>) -> Self {
        match raw.map(String::as_str) {
            Some("primary") => Role::Primary,
            Some("observer") => Role::Observer,
            _ => Role::Auto,
        }
    }
}

/// Per-session snapshot used to resync newly connected renderers.
struct Snapshot {
    media: PlaybackMediaPayload,
    player_state: PlayerState,
    current_time: f64,
    duration: Option<f64>,
    idle_reason: Option<IdleReason>,
}

/// A live renderer connection tracked by the actor.
struct Conn {
    id: u64,
    role: Role,
    is_primary: bool,
    out: mpsc::Sender<String>,
}

/// Messages driving the connection actor.
enum BridgeMsg {
    Register {
        id: u64,
        role: Role,
        out: mpsc::Sender<String>,
    },
    Unregister {
        id: u64,
    },
    Report {
        id: u64,
        report: PlayerReport,
    },
    Command(PlayerCommand),
    #[cfg(test)]
    Count(tokio::sync::oneshot::Sender<usize>),
}

/// The single-task owner of the connection registry and resync snapshots.
struct BridgeActor {
    connections: Vec<Conn>,
    primary: Option<u64>,
    snapshots: HashMap<String, Snapshot>,
    reports_tx: mpsc::Sender<PlayerReport>,
}

impl BridgeActor {
    async fn run(mut self, mut rx: mpsc::Receiver<BridgeMsg>) {
        while let Some(msg) = rx.recv().await {
            match msg {
                BridgeMsg::Register { id, role, out } => self.register(id, role, out).await,
                BridgeMsg::Unregister { id } => self.unregister(id),
                BridgeMsg::Report { id, report } => self.report(id, report).await,
                BridgeMsg::Command(command) => self.command(command).await,
                #[cfg(test)]
                BridgeMsg::Count(reply) => {
                    let _ = reply.send(self.connections.len());
                }
            }
        }
    }

    async fn register(&mut self, id: u64, role: Role, out: mpsc::Sender<String>) {
        let mut conn = Conn {
            id,
            role,
            is_primary: false,
            out,
        };
        self.assign_primary(&mut conn);
        tracing::info!(
            conn_id = id,
            role = ?role,
            is_primary = conn.is_primary,
            total = self.connections.len() + 1,
            "renderer connected"
        );
        self.connections.push(conn);
        self.sync(id).await;
    }

    fn assign_primary(&mut self, conn: &mut Conn) {
        if conn.role == Role::Observer {
            return;
        }
        if self.primary.is_none() {
            self.primary = Some(conn.id);
            conn.is_primary = true;
            return;
        }
        if conn.role == Role::Primary {
            if let Some(old) = self.primary {
                if let Some(existing) = self.connections.iter_mut().find(|c| c.id == old) {
                    existing.is_primary = false;
                }
            }
            self.primary = Some(conn.id);
            conn.is_primary = true;
        }
    }

    fn unregister(&mut self, id: u64) {
        let was_primary = self.primary == Some(id);
        self.connections.retain(|c| c.id != id);
        if was_primary {
            self.primary = None;
            self.promote_primary();
        }
        tracing::info!(
            conn_id = id,
            was_primary,
            remaining = self.connections.len(),
            "renderer disconnected"
        );
    }

    fn promote_primary(&mut self) {
        if let Some(candidate) = self
            .connections
            .iter_mut()
            .find(|c| c.role != Role::Observer)
        {
            candidate.is_primary = true;
            self.primary = Some(candidate.id);
        }
    }

    async fn report(&mut self, id: u64, report: PlayerReport) {
        if self.primary != Some(id) {
            return;
        }
        let session_id = report.session_id().to_string();
        let Some(snapshot) = self.snapshots.get_mut(&session_id) else {
            return;
        };
        if let PlayerReport::State {
            player_state,
            current_time,
            duration,
            idle_reason,
            ..
        } = &report
        {
            snapshot.player_state = *player_state;
            snapshot.current_time = *current_time;
            snapshot.duration = *duration;
            snapshot.idle_reason = *idle_reason;
        }
        tracing::debug!(conn_id = id, session_id = %session_id, report = ?report, "forwarding primary report");
        let _ = self.reports_tx.send(report).await;
    }

    async fn command(&mut self, command: PlayerCommand) {
        match &command {
            PlayerCommand::Load { session_id, media } => {
                tracing::info!(
                    session_id = %session_id,
                    streams = media.streams.len(),
                    first_url = %media.streams.first().map(|s| s.url.as_str()).unwrap_or(""),
                    "dispatching load to renderers"
                );
            }
            PlayerCommand::Stop { session_id } => {
                tracing::info!(session_id = %session_id, "dispatching stop to renderers");
            }
            PlayerCommand::Play { session_id }
            | PlayerCommand::Pause { session_id }
            | PlayerCommand::Seek { session_id, .. }
            | PlayerCommand::Volume { session_id, .. } => {
                tracing::debug!(session_id = %session_id, cmd = ?command, "dispatching player command");
            }
        }
        self.update_snapshot(&command);
        let text = match serde_json::to_string(&command) {
            Ok(text) => text,
            Err(error) => {
                tracing::error!(%error, "failed to serialize player command");
                return;
            }
        };
        self.broadcast(&text).await;
    }

    fn update_snapshot(&mut self, command: &PlayerCommand) {
        match command {
            PlayerCommand::Load { session_id, media } => {
                self.snapshots.insert(
                    session_id.clone(),
                    Snapshot {
                        media: media.clone(),
                        player_state: PlayerState::Buffering,
                        current_time: media.start_time,
                        duration: media.duration,
                        idle_reason: None,
                    },
                );
            }
            PlayerCommand::Play { session_id } => {
                if let Some(snapshot) = self.snapshots.get_mut(session_id) {
                    snapshot.player_state = PlayerState::Playing;
                    snapshot.idle_reason = None;
                }
            }
            PlayerCommand::Pause { session_id } => {
                if let Some(snapshot) = self.snapshots.get_mut(session_id) {
                    snapshot.player_state = PlayerState::Paused;
                    snapshot.idle_reason = None;
                }
            }
            PlayerCommand::Seek {
                session_id,
                position,
            } => {
                if let Some(snapshot) = self.snapshots.get_mut(session_id) {
                    snapshot.current_time = *position;
                }
            }
            PlayerCommand::Stop { session_id } => {
                self.snapshots.remove(session_id);
            }
            PlayerCommand::Volume { .. } => {}
        }
    }

    async fn broadcast(&mut self, text: &str) {
        let mut dead = Vec::new();
        for conn in &self.connections {
            if conn.out.send(text.to_string()).await.is_err() {
                dead.push(conn.id);
            }
        }
        for id in dead {
            self.unregister(id);
        }
    }

    async fn sync(&self, conn_id: u64) {
        let Some(conn) = self.connections.iter().find(|c| c.id == conn_id) else {
            return;
        };
        for (session_id, snapshot) in &self.snapshots {
            if snapshot.player_state == PlayerState::Idle {
                continue;
            }
            let mut commands = vec![PlayerCommand::Load {
                session_id: session_id.clone(),
                media: snapshot.media.clone(),
            }];
            if snapshot.current_time > 0.0 {
                commands.push(PlayerCommand::Seek {
                    session_id: session_id.clone(),
                    position: snapshot.current_time,
                });
            }
            match snapshot.player_state {
                PlayerState::Playing | PlayerState::Buffering => {
                    commands.push(PlayerCommand::Play {
                        session_id: session_id.clone(),
                    });
                }
                PlayerState::Paused => {
                    commands.push(PlayerCommand::Pause {
                        session_id: session_id.clone(),
                    });
                }
                PlayerState::Idle => {}
            }
            for command in commands {
                if let Ok(text) = serde_json::to_string(&command) {
                    let _ = conn.out.send(text).await;
                }
            }
        }
    }
}

/// Shared, cheaply-cloneable bridge state (axum handler state + renderer seam).
#[derive(Clone)]
struct BridgeState {
    commands: mpsc::Sender<BridgeMsg>,
    licenses: Arc<Mutex<HashMap<String, Arc<dyn LicenseHandler>>>>,
    manifests: Arc<Mutex<HashMap<String, Arc<dyn ManifestHandler>>>>,
    next_id: Arc<AtomicU64>,
    resolved_host: Arc<str>,
    configured_port: u16,
    port: Arc<AtomicU16>,
}

impl BridgeState {
    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::SeqCst)
    }

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

/// HTTP/WebSocket bridge relaying player commands to external renderers.
pub struct PlayerBridge {
    state: BridgeState,
    bind_host: Arc<str>,
    server: Mutex<Option<JoinHandle<()>>>,
}

impl PlayerBridge {
    /// Create a bridge, spawning the connection actor. Player reports from the
    /// primary renderer are delivered on `reports`.
    #[must_use]
    pub fn new(host: impl Into<String>, port: u16, reports: mpsc::Sender<PlayerReport>) -> Self {
        let host = host.into();
        let resolved_host = if host == "0.0.0.0" || host == "::" {
            "127.0.0.1".to_string()
        } else {
            host.clone()
        };

        let (commands_tx, commands_rx) = mpsc::channel(128);
        let actor = BridgeActor {
            connections: Vec::new(),
            primary: None,
            snapshots: HashMap::new(),
            reports_tx: reports,
        };
        tokio::spawn(actor.run(commands_rx));

        let state = BridgeState {
            commands: commands_tx,
            licenses: Arc::new(Mutex::new(HashMap::new())),
            manifests: Arc::new(Mutex::new(HashMap::new())),
            next_id: Arc::new(AtomicU64::new(1)),
            resolved_host: Arc::from(resolved_host.as_str()),
            configured_port: port,
            port: Arc::new(AtomicU16::new(0)),
        };

        Self {
            state,
            bind_host: Arc::from(host.as_str()),
            server: Mutex::new(None),
        }
    }

    /// Bind the listener and start serving. Idempotent-safe to call once.
    pub async fn start(&self) -> std::io::Result<()> {
        let listener =
            tokio::net::TcpListener::bind((self.bind_host.as_ref(), self.state.configured_port))
                .await?;
        let port = listener.local_addr()?.port();
        self.state.port.store(port, Ordering::SeqCst);

        let app = router(self.state.clone());
        let handle = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        *self.server.lock().unwrap() = Some(handle);

        tracing::info!(
            host = %self.state.resolved_host,
            port,
            "player bridge started (web=http://{}:{}/)",
            self.state.resolved_host,
            port
        );
        Ok(())
    }

    /// Stop serving and clear session handlers.
    pub async fn stop(&self) {
        if let Some(handle) = self.server.lock().unwrap().take() {
            handle.abort();
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

#[async_trait]
impl Renderer for PlayerBridge {
    async fn send(&self, command: PlayerCommand) {
        let _ = self.state.commands.send(BridgeMsg::Command(command)).await;
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

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<BridgeState>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let role = Role::parse(params.get("role"));
    ws.on_upgrade(move |socket| handle_socket(socket, state, role))
}

async fn handle_socket(socket: WebSocket, state: BridgeState, role: Role) {
    let id = state.next_id();
    let (out_tx, mut out_rx) = mpsc::channel::<String>(64);
    let (mut sink, mut stream) = socket.split();

    if state
        .commands
        .send(BridgeMsg::Register {
            id,
            role,
            out: out_tx,
        })
        .await
        .is_err()
    {
        return;
    }

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
            incoming = stream.next() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<PlayerReport>(text.as_str()) {
                            Ok(report) => {
                                let _ = state.commands.send(BridgeMsg::Report { id, report }).await;
                            }
                            Err(_) => tracing::warn!("invalid player report payload"),
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(_)) => break,
                }
            }
        }
    }

    let _ = state.commands.send(BridgeMsg::Unregister { id }).await;
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
    let request = LicenseRequest {
        session_id,
        body: body.to_vec(),
        content_type,
        route_id: params.get("route").cloned(),
        headers: filter_upstream_headers(&header_map_to_map(&request_headers)),
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

    let route_id = route_path.split('.').next().unwrap_or("").to_string();
    if route_id.is_empty() {
        return StatusCode::BAD_REQUEST.into_response();
    }

    let is_head = method == Method::HEAD;
    let request = ManifestProxyRequest {
        session_id,
        route_id,
        method: method.as_str().to_string(),
        headers: filter_upstream_headers(&header_map_to_map(&request_headers)),
    };

    match handler.handle_manifest(request).await {
        Ok(response) => {
            let mut builder = Response::builder()
                .status(response.status)
                .header(CONTENT_TYPE, response.content_type);
            for (name, value) in filter_upstream_response_headers(&response.headers) {
                // Drop upstream caching directives; the proxy URL is reused
                // across stream switches (only the query version changes), so a
                // cached manifest would stall playback on the previous stream.
                let lowered = name.to_ascii_lowercase();
                if lowered == "cache-control" || lowered == "expires" || lowered == "etag" {
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

fn header_map_to_map(headers: &HeaderMap) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for (name, value) in headers {
        if let Ok(value) = value.to_str() {
            map.insert(name.as_str().to_string(), value.to_string());
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::body::to_bytes;
    use axum::http::Request;
    use serde_json::{json, Value};
    use tokio::sync::oneshot;
    use tokio_tungstenite::connect_async;
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    use super::*;
    use crate::protocol::{PlaybackStreamPayload, PlayerCommand};
    use crate::proxy::{LicenseResponse, ManifestProxyResponse, ProxyError, ProxyResult};
    use vibecast_messages::StreamType;

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

    fn bridge() -> (PlayerBridge, mpsc::Receiver<PlayerReport>) {
        let (reports_tx, reports_rx) = mpsc::channel(16);
        (PlayerBridge::new("127.0.0.1", 0, reports_tx), reports_rx)
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
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
    }

    #[tokio::test]
    async fn serves_default_shaka_player_page() {
        let (bridge, _reports) = bridge();
        let (status, headers, body) = http_get(&bridge, "/").await;
        let body = String::from_utf8(body).unwrap();

        assert_eq!(status, StatusCode::OK);
        assert!(content_type(&headers).starts_with("text/html"));
        assert!(body.contains("shaka-player.compiled.js"));
        assert!(body.contains(r#"src="/player.js""#));
        assert!(body.contains(r#"<video id="video" class="video" playsinline></video>"#));
        assert!(body.contains("/player?role=primary"));
    }

    #[tokio::test]
    async fn serves_player_script() {
        let (bridge, _reports) = bridge();
        let (status, headers, body) = http_get(&bridge, "/player.js").await;
        let body = String::from_utf8(body).unwrap();

        assert_eq!(status, StatusCode::OK);
        assert!(content_type(&headers).starts_with("application/javascript"));
        assert!(body.contains("new shaka.Player"));
        assert!(body.contains("/player?role=primary"));
    }

    #[tokio::test]
    async fn start_and_stop() {
        let (bridge, _reports) = bridge();
        bridge.start().await.unwrap();
        assert!(bridge.serving_port().is_some());
        bridge.stop().await;
        assert!(bridge.serving_port().is_none());
    }

    async fn connect(
        port: u16,
        role: &str,
    ) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>
    {
        let url = format!("ws://127.0.0.1:{port}/player?role={role}");
        let (ws, _response) = connect_async(url).await.unwrap();
        ws
    }

    async fn next_json<S>(ws: &mut S) -> Value
    where
        S: StreamExt<Item = Result<WsMessage, tokio_tungstenite::tungstenite::Error>> + Unpin,
    {
        let message = tokio::time::timeout(Duration::from_secs(1), ws.next())
            .await
            .expect("timed out waiting for message")
            .expect("stream ended")
            .expect("ws error");
        match message {
            WsMessage::Text(text) => serde_json::from_str(text.as_str()).unwrap(),
            other => panic!("expected text frame, got {other:?}"),
        }
    }

    async fn wait_for_connections(bridge: &PlayerBridge, target: usize) {
        for _ in 0..200 {
            let (tx, rx) = oneshot::channel();
            if bridge
                .state
                .commands
                .send(BridgeMsg::Count(tx))
                .await
                .is_err()
            {
                return;
            }
            if rx.await.unwrap_or(0) >= target {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("connections did not reach {target}");
    }

    #[tokio::test]
    async fn command_fanout_and_primary_state_reporting() {
        let (bridge, mut reports) = bridge();
        bridge.start().await.unwrap();
        let port = bridge.serving_port().unwrap();

        let mut primary = connect(port, "primary").await;
        let mut observer = connect(port, "observer").await;
        wait_for_connections(&bridge, 2).await;

        bridge
            .send(PlayerCommand::Load {
                session_id: "session-1".into(),
                media: media(),
            })
            .await;

        let primary_load = next_json(&mut primary).await;
        let observer_load = next_json(&mut observer).await;
        assert_eq!(primary_load["type"], "load");
        assert_eq!(observer_load["type"], "load");
        assert_eq!(
            primary_load["media"]["streams"][0]["url"],
            "https://example.com/manifest.mpd"
        );

        // Reports from a non-primary connection are ignored.
        observer
            .send(WsMessage::text(
                json!({"type":"state","sessionId":"session-1","playerState":"PLAYING","currentTime":11}).to_string(),
            ))
            .await
            .unwrap();

        // Reports from the primary are forwarded.
        primary
            .send(WsMessage::text(
                json!({"type":"state","sessionId":"session-1","playerState":"PLAYING","currentTime":21.5}).to_string(),
            ))
            .await
            .unwrap();

        let report = tokio::time::timeout(Duration::from_secs(1), reports.recv())
            .await
            .expect("timed out")
            .expect("channel closed");
        match report {
            PlayerReport::State {
                player_state,
                current_time,
                ..
            } => {
                assert_eq!(player_state, PlayerState::Playing);
                assert_eq!(current_time, 21.5);
            }
            PlayerReport::Error { .. } => panic!("expected state report"),
        }
        // The observer's report must not have been forwarded.
        assert!(
            tokio::time::timeout(Duration::from_millis(100), reports.recv())
                .await
                .is_err()
        );

        bridge.stop().await;
    }

    #[tokio::test]
    async fn auto_sync_on_connect() {
        let (bridge, _reports) = bridge();
        bridge.start().await.unwrap();
        let port = bridge.serving_port().unwrap();

        bridge
            .send(PlayerCommand::Load {
                session_id: "session-1".into(),
                media: media(),
            })
            .await;
        bridge
            .send(PlayerCommand::Seek {
                session_id: "session-1".into(),
                position: 42.0,
            })
            .await;
        bridge
            .send(PlayerCommand::Pause {
                session_id: "session-1".into(),
            })
            .await;

        let mut ws = connect(port, "auto").await;
        let first = next_json(&mut ws).await;
        let second = next_json(&mut ws).await;
        let third = next_json(&mut ws).await;

        assert_eq!(first["type"], "load");
        assert_eq!(second["type"], "seek");
        assert_eq!(second["position"], 42.0);
        assert_eq!(third["type"], "pause");

        bridge.stop().await;
    }

    // -- proxy handlers ----------------------------------------------------

    #[derive(Clone, Default)]
    struct RecordingLicense {
        requests: Arc<Mutex<Vec<LicenseRequest>>>,
    }

    #[async_trait]
    impl LicenseHandler for RecordingLicense {
        async fn handle_license(&self, request: LicenseRequest) -> ProxyResult<LicenseResponse> {
            self.requests.lock().unwrap().push(request.clone());
            let mut body = request.body;
            body.extend_from_slice(b"-ok");
            Ok(LicenseResponse::ok(body))
        }
    }

    #[tokio::test]
    async fn license_proxy_round_trip() {
        let (bridge, _reports) = bridge();
        let handler = RecordingLicense::default();
        bridge.register_license_handler("session-1", Arc::new(handler.clone()));

        let request = Request::builder()
            .method("POST")
            .uri("/license/session-1?route=r7")
            .header(CONTENT_TYPE, "application/octet-stream")
            .body(Body::from(b"challenge".to_vec()))
            .unwrap();
        let (status, headers, body) = drive(&bridge, request).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(content_type(&headers), "application/octet-stream");
        assert_eq!(body, b"challenge-ok");
        let recorded = handler.requests.lock().unwrap();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].session_id, "session-1");
        assert_eq!(recorded[0].route_id.as_deref(), Some("r7"));
        assert_eq!(recorded[0].body, b"challenge");
    }

    #[tokio::test]
    async fn license_proxy_missing_handler_returns_404() {
        let (bridge, _reports) = bridge();
        let request = Request::builder()
            .method("POST")
            .uri("/license/missing")
            .body(Body::from(b"x".to_vec()))
            .unwrap();
        let (status, _headers, _body) = drive(&bridge, request).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    struct ForbiddenLicense;

    #[async_trait]
    impl LicenseHandler for ForbiddenLicense {
        async fn handle_license(&self, _request: LicenseRequest) -> ProxyResult<LicenseResponse> {
            Ok(LicenseResponse {
                body: b"forbidden".to_vec(),
                content_type: "text/plain".into(),
                status: 403,
            })
        }
    }

    #[tokio::test]
    async fn license_proxy_preserves_explicit_error_response() {
        let (bridge, _reports) = bridge();
        bridge.register_license_handler("session-1", Arc::new(ForbiddenLicense));

        let request = Request::builder()
            .method("POST")
            .uri("/license/session-1")
            .body(Body::from(b"challenge".to_vec()))
            .unwrap();
        let (status, headers, body) = drive(&bridge, request).await;

        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(content_type(&headers), "text/plain");
        assert_eq!(body, b"forbidden");
    }

    struct FailingLicense;

    #[async_trait]
    impl LicenseHandler for FailingLicense {
        async fn handle_license(&self, _request: LicenseRequest) -> ProxyResult<LicenseResponse> {
            Err(ProxyError("boom".into()))
        }
    }

    #[tokio::test]
    async fn license_proxy_unhandled_error_returns_500() {
        let (bridge, _reports) = bridge();
        bridge.register_license_handler("session-1", Arc::new(FailingLicense));

        let request = Request::builder()
            .method("POST")
            .uri("/license/session-1")
            .body(Body::from(b"challenge".to_vec()))
            .unwrap();
        let (status, _headers, _body) = drive(&bridge, request).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[derive(Clone, Default)]
    struct RecordingManifest {
        requests: Arc<Mutex<Vec<ManifestProxyRequest>>>,
    }

    #[async_trait]
    impl ManifestHandler for RecordingManifest {
        async fn handle_manifest(
            &self,
            request: ManifestProxyRequest,
        ) -> ProxyResult<ManifestProxyResponse> {
            self.requests.lock().unwrap().push(request);
            Ok(ManifestProxyResponse {
                body: b"#EXTM3U\n".to_vec(),
                content_type: "application/vnd.apple.mpegurl".into(),
                status: 200,
                headers: HashMap::from([("Cache-Control".to_string(), "no-store".to_string())]),
            })
        }
    }

    #[tokio::test]
    async fn manifest_proxy_round_trip() {
        let (bridge, _reports) = bridge();
        let handler = RecordingManifest::default();
        bridge.register_manifest_handler("session-1", Arc::new(handler.clone()));

        let (status, headers, body) = http_get(&bridge, "/manifest/session-1/m7.m3u8").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(content_type(&headers), "application/vnd.apple.mpegurl");
        // The proxy forces no-store regardless of upstream directives so that
        // stream switches (which reuse the proxy URL) always re-fetch.
        assert_eq!(
            headers.get("Cache-Control").and_then(|v| v.to_str().ok()),
            Some("no-store, max-age=0")
        );
        assert_eq!(body, b"#EXTM3U\n");
        {
            let recorded = handler.requests.lock().unwrap();
            assert_eq!(recorded.len(), 1);
            assert_eq!(recorded[0].route_id, "m7");
            assert_eq!(recorded[0].method, "GET");
        }

        let (missing, _headers, _body) = http_get(&bridge, "/manifest/missing/m0.mpd").await;
        assert_eq!(missing, StatusCode::NOT_FOUND);
    }

    struct HeadManifest;

    #[async_trait]
    impl ManifestHandler for HeadManifest {
        async fn handle_manifest(
            &self,
            request: ManifestProxyRequest,
        ) -> ProxyResult<ManifestProxyResponse> {
            assert_eq!(request.method, "HEAD");
            Ok(ManifestProxyResponse {
                body: b"ignored".to_vec(),
                content_type: "application/dash+xml".into(),
                status: 200,
                headers: HashMap::new(),
            })
        }
    }

    #[tokio::test]
    async fn manifest_proxy_head_request() {
        let (bridge, _reports) = bridge();
        bridge.register_manifest_handler("session-1", Arc::new(HeadManifest));

        let request = Request::builder()
            .method("HEAD")
            .uri("/manifest/session-1/m0.mpd")
            .body(Body::empty())
            .unwrap();
        let (status, headers, body) = drive(&bridge, request).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(content_type(&headers), "application/dash+xml");
        assert!(body.is_empty());
    }

    struct NoisyManifest;

    #[async_trait]
    impl ManifestHandler for NoisyManifest {
        async fn handle_manifest(
            &self,
            _request: ManifestProxyRequest,
        ) -> ProxyResult<ManifestProxyResponse> {
            let headers = HashMap::from([
                (
                    "Connection".to_string(),
                    "keep-alive, X-Remove-Me".to_string(),
                ),
                ("Content-Encoding".to_string(), "gzip".to_string()),
                ("Content-Length".to_string(), "999".to_string()),
                ("Content-Type".to_string(), "text/plain".to_string()),
                ("Set-Cookie".to_string(), "sid=123".to_string()),
                ("Transfer-Encoding".to_string(), "chunked".to_string()),
                ("X-Remove-Me".to_string(), "1".to_string()),
                ("X-Preserved".to_string(), "ok".to_string()),
            ]);
            Ok(ManifestProxyResponse {
                body: b"#EXTM3U\n".to_vec(),
                content_type: "application/vnd.apple.mpegurl".into(),
                status: 200,
                headers,
            })
        }
    }

    #[tokio::test]
    async fn manifest_proxy_filters_hop_by_hop_response_headers() {
        let (bridge, _reports) = bridge();
        bridge.register_manifest_handler("session-1", Arc::new(NoisyManifest));

        let (status, headers, _body) = http_get(&bridge, "/manifest/session-1/m0.m3u8").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(content_type(&headers), "application/vnd.apple.mpegurl");
        assert_eq!(
            headers.get("X-Preserved").and_then(|v| v.to_str().ok()),
            Some("ok")
        );
        for blocked in [
            "Connection",
            "Content-Encoding",
            "Set-Cookie",
            "Transfer-Encoding",
            "X-Remove-Me",
        ] {
            assert!(
                !headers.contains_key(blocked),
                "{blocked} should be dropped"
            );
        }
    }
}
