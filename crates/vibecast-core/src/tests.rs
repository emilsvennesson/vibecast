//! End-to-end tests: a real Cast connection + hub driving a fake app,
//! fake player, and fake proxy registrar over an in-memory duplex stream.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::io::DuplexStream;
use tokio::sync::mpsc;
use tokio_util::codec::Framed;

use vibecast_cast::{message, namespace as ns, run_connection, AuthMaterial, ServerEvent};
use vibecast_messages::{PlayerState, Volume};
use vibecast_player_api::{LicenseHandler, ManifestHandler, Player, PlayerCommand, PlayerReport};
use vibecast_proto::CastCodec;
use vibecast_sdk::{
    AppContext, AppProvider, AppSession, LaunchCredentials, LaunchError, LoadRequest,
    MediaResolveError, PlaybackMedia, PlaybackStream, PlayerCapabilities, ReceiverContext,
    StreamType,
};
use vibecast_security::CertificateBundle;

use crate::{AppRegistry, DeviceHub, DeviceHubHandle, DeviceIdentity, HubConfig, ProxyRegistrar};

// -- fakes -----------------------------------------------------------------

#[derive(Default)]
struct FakePlayer {
    commands: Mutex<Vec<PlayerCommand>>,
}

#[async_trait]
impl Player for FakePlayer {
    async fn send(&self, command: PlayerCommand) {
        self.commands.lock().unwrap().push(command);
    }
}

impl FakePlayer {
    fn commands(&self) -> Vec<PlayerCommand> {
        self.commands.lock().unwrap().clone()
    }
}

#[derive(Default)]
struct FakeProxy {
    events: Mutex<Vec<String>>,
}

impl ProxyRegistrar for FakeProxy {
    fn register_license(&self, session_id: &str, _handler: Arc<dyn LicenseHandler>) -> String {
        self.events
            .lock()
            .unwrap()
            .push(format!("+license:{session_id}"));
        format!("http://proxy/license/{session_id}")
    }
    fn unregister_license(&self, session_id: &str) {
        self.events
            .lock()
            .unwrap()
            .push(format!("-license:{session_id}"));
    }
    fn register_manifest(&self, session_id: &str, _handler: Arc<dyn ManifestHandler>) -> String {
        self.events
            .lock()
            .unwrap()
            .push(format!("+manifest:{session_id}"));
        format!("http://proxy/manifest/{session_id}")
    }
    fn unregister_manifest(&self, session_id: &str) {
        self.events
            .lock()
            .unwrap()
            .push(format!("-manifest:{session_id}"));
    }
}

struct FakeApp;

#[async_trait]
impl AppProvider for FakeApp {
    fn app_ids(&self) -> &'static [&'static str] {
        &["APP1"]
    }
    fn display_name(&self) -> &'static str {
        "Fake App"
    }
    fn app_key(&self) -> &'static str {
        "fake"
    }
    fn namespaces(&self) -> &'static [&'static str] {
        &[FAKE_NS]
    }
    async fn launch(
        &self,
        _ctx: &AppContext,
        _credentials: LaunchCredentials,
    ) -> Result<Arc<dyn AppSession>, LaunchError> {
        Ok(Arc::new(FakeSession))
    }
}

const FAKE_NS: &str = "urn:x-cast:test.fake";

struct FakeSession;

#[async_trait]
impl AppSession for FakeSession {
    async fn resolve_media(
        &self,
        ctx: &AppContext,
        request: &LoadRequest,
    ) -> Result<PlaybackMedia, MediaResolveError> {
        if request.media.content_id == "fail" {
            return Err(MediaResolveError::content_unavailable("NO_CONTENT"));
        }
        let mut media = PlaybackMedia::new(
            ctx.session_id.clone(),
            vec![PlaybackStream::url(
                "https://cdn.example/manifest.mpd",
                "application/dash+xml",
            )],
            StreamType::Buffered,
        );
        media.title = Some("Fake Title".into());
        media.duration = Some(120.0);
        media.start_time = request.current_time;
        Ok(media)
    }

    async fn on_message(
        &self,
        ctx: &AppContext,
        namespace: &str,
        data: &Value,
    ) -> vibecast_sdk::MessageDisposition {
        if namespace == FAKE_NS {
            match data.get("type").and_then(Value::as_str) {
                Some("PUSH_MEDIA") => {
                    let mut media = PlaybackMedia::new(
                        ctx.session_id.clone(),
                        vec![PlaybackStream::url(
                            "https://cdn.example/video.mp4",
                            "video/mp4",
                        )],
                        StreamType::Buffered,
                    );
                    media.title = Some("App-driven media".into());
                    media.start_time = 7.0;
                    ctx.playback_controller().load(media).await;
                }
                Some("APP_PLAY") => ctx.playback_controller().play().await,
                Some("APP_PAUSE") => ctx.playback_controller().pause().await,
                Some("APP_SEEK") => ctx.playback_controller().seek(33.0).await,
                Some("APP_STOP") => ctx.playback_controller().stop().await,
                message_type => {
                    ctx.send_custom(
                        namespace,
                        serde_json::json!({"type": "PONG", "echo": message_type}),
                    )
                    .await;
                }
            }
            vibecast_sdk::MessageDisposition::Handled
        } else {
            vibecast_sdk::MessageDisposition::Unhandled
        }
    }
}

// -- harness ---------------------------------------------------------------

fn dummy_auth() -> AuthMaterial {
    AuthMaterial {
        bundle: CertificateBundle {
            peer_cert_pem: Vec::new(),
            peer_key_pem: Vec::new(),
            peer_cert_der: vec![1],
            device_cert_der: vec![1],
            intermediate_certs_der: Vec::new(),
            signature_sha1: Vec::new(),
            signature_sha256: Vec::new(),
            not_valid_before: 0,
            not_valid_after: i64::MAX,
            crl: None,
        },
        crl: None,
    }
}

struct Harness {
    client: Framed<DuplexStream, CastCodec>,
    hub: DeviceHubHandle,
    player: Arc<FakePlayer>,
    proxy: Arc<FakeProxy>,
}

fn attenuation_volume() -> Volume {
    Volume {
        level: 1.0,
        muted: false,
        control_type: Some("attenuation".into()),
        step_interval: Some(0.05),
    }
}

async fn setup() -> Harness {
    let (server_end, client_end) = tokio::io::duplex(64 * 1024);
    let (events_tx, mut events_rx) = mpsc::channel::<ServerEvent>(32);
    tokio::spawn(run_connection(
        server_end,
        1,
        Arc::from("peer"),
        Arc::new(dummy_auth()),
        events_tx,
    ));

    let player = Arc::new(FakePlayer::default());
    let proxy = Arc::new(FakeProxy::default());
    let hub = DeviceHub::new(HubConfig {
        identity: DeviceIdentity::new("Living Room".into(), "Chromecast".into(), "dev-1".into()),
        registry: AppRegistry::new(vec![Arc::new(FakeApp)]).expect("registry"),
        player: player.clone(),
        proxy: proxy.clone(),
        http: reqwest::Client::new(),
        data_dir: std::env::temp_dir().join("vibecast-core-tests"),
        volume: attenuation_volume(),
        user_agent: String::new(),
        cast_device_capabilities: String::new(),
        capabilities: PlayerCapabilities::default(),
    });
    let hub_handle = hub.handle();
    {
        let hub_handle = hub_handle.clone();
        tokio::spawn(async move {
            while let Some(event) = events_rx.recv().await {
                if hub_handle.send_server_event(event).await.is_err() {
                    break;
                }
            }
        });
    }
    tokio::spawn(hub.run());

    Harness {
        client: Framed::new(client_end, CastCodec),
        hub: hub_handle,
        player,
        proxy,
    }
}

async fn send(
    client: &mut Framed<DuplexStream, CastCodec>,
    namespace: &str,
    dest: &str,
    json: &str,
) {
    client
        .send(message::build_string(
            "sender-1",
            dest,
            namespace,
            json.to_string(),
        ))
        .await
        .unwrap();
}

async fn next_json(client: &mut Framed<DuplexStream, CastCodec>) -> Value {
    let message = client.next().await.unwrap().unwrap();
    serde_json::from_str(message.payload_utf8.as_deref().unwrap()).unwrap()
}

async fn launch(client: &mut Framed<DuplexStream, CastCodec>) -> String {
    send(
        client,
        ns::CONNECTION,
        "receiver-0",
        r#"{"type":"CONNECT"}"#,
    )
    .await;
    send(
        client,
        ns::RECEIVER,
        "receiver-0",
        r#"{"type":"LAUNCH","requestId":1,"appId":"APP1"}"#,
    )
    .await;
    let status = next_json(client).await;
    assert_eq!(status["type"], "RECEIVER_STATUS");
    let app = &status["status"]["applications"][0];
    assert_eq!(app["appId"], "APP1");
    assert_eq!(app["displayName"], "Fake App");
    app["transportId"].as_str().unwrap().to_string()
}

// -- tests -----------------------------------------------------------------

#[tokio::test]
async fn launch_load_and_play_end_to_end() {
    let mut harness = setup().await;
    let client = &mut harness.client;
    let transport = launch(client).await;

    // Subscribe to the app transport; the hub replies with the current (empty) status.
    send(client, ns::CONNECTION, &transport, r#"{"type":"CONNECT"}"#).await;
    let connect_status = next_json(client).await;
    assert_eq!(connect_status["type"], "MEDIA_STATUS");
    assert_eq!(connect_status["status"], serde_json::json!([]));

    // LOAD -> IDLE/LOADING, then resolved LOADING, then BUFFERING.
    send(
        client,
        ns::MEDIA,
        &transport,
        r#"{"type":"LOAD","requestId":2,"media":{"contentId":"abc","contentType":"video/mp4"}}"#,
    )
    .await;

    let loading = next_json(client).await;
    assert_eq!(loading["status"][0]["playerState"], "IDLE");
    assert_eq!(
        loading["status"][0]["extendedStatus"]["playerState"],
        "LOADING"
    );

    let resolved_loading = next_json(client).await;
    assert_eq!(
        resolved_loading["status"][0]["extendedStatus"]["playerState"],
        "LOADING"
    );

    let buffering = next_json(client).await;
    assert_eq!(buffering["status"][0]["playerState"], "BUFFERING");
    // The DASH stream URL was rewritten to the manifest proxy.
    let content_url = buffering["status"][0]["media"]["contentUrl"]
        .as_str()
        .unwrap();
    assert!(content_url.contains("/manifest/"), "url = {content_url}");
    assert_eq!(
        buffering["status"][0]["media"]["metadata"]["title"],
        "Fake Title"
    );

    // PLAY -> PLAYING.
    send(
        client,
        ns::MEDIA,
        &transport,
        r#"{"type":"PLAY","requestId":3,"mediaSessionId":2}"#,
    )
    .await;
    let playing = next_json(client).await;
    assert_eq!(playing["status"][0]["playerState"], "PLAYING");
    assert_eq!(playing["status"][0]["supportedMediaCommands"], 15);

    // The player received Load then Play; the manifest proxy was registered.
    let commands = harness.player.commands();
    assert!(matches!(commands.first(), Some(PlayerCommand::Load { .. })));
    assert!(matches!(commands.last(), Some(PlayerCommand::Play { .. })));
    if let Some(PlayerCommand::Load { media, .. }) = commands.first() {
        assert!(media.streams[0].url.contains("/manifest/"));
    }
    assert!(harness
        .proxy
        .events
        .lock()
        .unwrap()
        .iter()
        .any(|event| event.starts_with("+manifest:")));
}

#[tokio::test]
async fn load_failure_sends_load_failed() {
    let mut harness = setup().await;
    let client = &mut harness.client;
    let transport = launch(client).await;
    send(client, ns::CONNECTION, &transport, r#"{"type":"CONNECT"}"#).await;
    let _ = next_json(client).await; // connect status

    send(
        client,
        ns::MEDIA,
        &transport,
        r#"{"type":"LOAD","requestId":9,"media":{"contentId":"fail","contentType":"video/mp4"}}"#,
    )
    .await;

    // IDLE/LOADING broadcast, then LOAD_FAILED to the sender, then an ERROR status.
    let loading = next_json(client).await;
    assert_eq!(
        loading["status"][0]["extendedStatus"]["playerState"],
        "LOADING"
    );

    let failed = next_json(client).await;
    assert_eq!(failed["type"], "LOAD_FAILED");
    assert_eq!(failed["requestId"], 9);
    assert_eq!(failed["reason"], "CONTENT_UNAVAILABLE");

    let idle = next_json(client).await;
    assert_eq!(idle["type"], "MEDIA_STATUS");
    assert_eq!(idle["status"][0]["idleReason"], "ERROR");
}

#[tokio::test]
async fn primary_player_report_broadcasts_status() {
    let mut harness = setup().await;
    let transport = {
        let client = &mut harness.client;
        let transport = launch(client).await;
        send(client, ns::CONNECTION, &transport, r#"{"type":"CONNECT"}"#).await;
        let _ = next_json(client).await; // connect status
                                         // LOAD so there is media to report against, then drain its 3 statuses.
        send(
            client,
            ns::MEDIA,
            &transport,
            r#"{"type":"LOAD","requestId":2,"media":{"contentId":"abc"}}"#,
        )
        .await;
        for _ in 0..3 {
            let _ = next_json(client).await;
        }
        transport
    };

    // A player state report from the player bridge.
    harness
        .hub
        .send_player_report(PlayerReport::State {
            session_id: transport.clone(),
            player_state: PlayerState::Playing,
            current_time: 33.5,
            duration: Some(120.0),
            idle_reason: None,
        })
        .await
        .unwrap();

    let status = next_json(&mut harness.client).await;
    assert_eq!(status["type"], "MEDIA_STATUS");
    assert_eq!(status["status"][0]["playerState"], "PLAYING");
    assert_eq!(status["status"][0]["currentTime"], 33.5);
}

#[tokio::test]
async fn receiver_status_lists_running_app() {
    let mut harness = setup().await;
    let client = &mut harness.client;
    let transport = launch(client).await;
    // A sender subscribing to the app transport marks the app sender-connected.
    send(client, ns::CONNECTION, &transport, r#"{"type":"CONNECT"}"#).await;
    let _ = next_json(client).await; // connect status

    send(
        client,
        ns::RECEIVER,
        "receiver-0",
        r#"{"type":"GET_STATUS","requestId":5}"#,
    )
    .await;
    let status = next_json(client).await;
    assert_eq!(status["type"], "RECEIVER_STATUS");
    assert_eq!(status["requestId"], 5);
    assert_eq!(status["status"]["applications"][0]["appId"], "APP1");
    assert_eq!(status["status"]["applications"][0]["senderConnected"], true);
}

#[tokio::test]
async fn custom_namespace_message_is_handled_and_replies() {
    let mut harness = setup().await;
    let client = &mut harness.client;
    let transport = launch(client).await;
    send(client, ns::CONNECTION, &transport, r#"{"type":"CONNECT"}"#).await;
    let _ = next_json(client).await; // connect status

    // A message on the app's custom namespace reaches on_message, which replies
    // to the sender via ctx.send_custom.
    send(client, FAKE_NS, &transport, r#"{"type":"PING"}"#).await;
    let reply = next_json(client).await;
    assert_eq!(reply["type"], "PONG");
    assert_eq!(reply["echo"], "PING");
}

#[tokio::test]
async fn app_driven_playback_uses_canonical_media_path() {
    let mut harness = setup().await;
    let client = &mut harness.client;
    let transport = launch(client).await;
    send(client, ns::CONNECTION, &transport, r#"{"type":"CONNECT"}"#).await;
    let _ = next_json(client).await;

    send(client, FAKE_NS, &transport, r#"{"type":"PUSH_MEDIA"}"#).await;
    let loading = next_json(client).await;
    let buffering = next_json(client).await;
    assert_eq!(
        loading["status"][0]["extendedStatus"]["playerState"],
        "LOADING"
    );
    assert_eq!(loading["status"][0]["mediaSessionId"], 2);
    assert_eq!(buffering["status"][0]["playerState"], "BUFFERING");
    assert_eq!(buffering["status"][0]["currentTime"], 7.0);

    send(client, FAKE_NS, &transport, r#"{"type":"APP_PAUSE"}"#).await;
    assert_eq!(
        next_json(client).await["status"][0]["playerState"],
        "PAUSED"
    );
    send(client, FAKE_NS, &transport, r#"{"type":"APP_SEEK"}"#).await;
    assert_eq!(next_json(client).await["status"][0]["currentTime"], 33.0);
    send(client, FAKE_NS, &transport, r#"{"type":"APP_PLAY"}"#).await;
    assert_eq!(
        next_json(client).await["status"][0]["playerState"],
        "PLAYING"
    );
    send(client, FAKE_NS, &transport, r#"{"type":"APP_STOP"}"#).await;
    let stopped = next_json(client).await;
    assert_eq!(stopped["status"][0]["playerState"], "IDLE");
    assert_eq!(stopped["status"][0]["idleReason"], "CANCELLED");

    let commands = harness.player.commands();
    assert!(matches!(commands[0], PlayerCommand::Load { .. }));
    assert!(matches!(commands[1], PlayerCommand::Pause { .. }));
    assert!(matches!(
        commands[2],
        PlayerCommand::Seek { position: 33.0, .. }
    ));
    assert!(matches!(commands[3], PlayerCommand::Play { .. }));
    assert!(matches!(commands[4], PlayerCommand::Stop { .. }));
}

/// An app session whose `resolve_license` reverses the challenge instead of
/// forwarding (proving the override is used, no network involved).
struct LicenseSession;

#[async_trait]
impl AppSession for LicenseSession {
    async fn resolve_media(
        &self,
        _ctx: &AppContext,
        _request: &LoadRequest,
    ) -> Result<PlaybackMedia, MediaResolveError> {
        Err(MediaResolveError::internal("UNUSED"))
    }

    async fn resolve_license(
        &self,
        _ctx: &AppContext,
        request: vibecast_sdk::LicenseRequest,
        _route: vibecast_sdk::LicenseRoute,
        _forward: &dyn vibecast_sdk::LicenseForwarder,
    ) -> vibecast_sdk::LicenseResponse {
        let mut body = request.body;
        body.reverse();
        vibecast_sdk::LicenseResponse {
            body,
            content_type: "application/xprotobuf".into(),
            status: 200,
        }
    }
}

#[tokio::test]
async fn app_resolve_license_override_is_used() {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use vibecast_player_api::{LicenseHandler, LicenseRequest as WireLicenseRequest, RouteId};

    use crate::proxy::{LicenseRoute, SessionProxy};

    let app: Arc<dyn AppSession> = Arc::new(LicenseSession);
    let ctx = AppContext::new(
        "s",
        "s",
        "APP1",
        reqwest::Client::new(),
        ReceiverContext::new("Living Room", "Chromecast", "dev-1", PathBuf::from("/tmp")),
        Arc::new(vibecast_sdk::NoopSenderChannel),
    );
    let license_routes = HashMap::from([(
        RouteId::license(0),
        LicenseRoute {
            system: vibecast_sdk::DrmSystem::ClearKey,
            upstream_url: "https://unused.example/license".into(),
            headers: http::HeaderMap::new(),
        },
    )]);
    let proxy = SessionProxy::new(app, ctx, HashMap::new(), license_routes);

    let request = WireLicenseRequest {
        session_id: "s".into(),
        body: b"abc".to_vec(),
        content_type: "application/octet-stream".into(),
        route_id: Some(RouteId::license(0)),
        headers: http::HeaderMap::new(),
    };
    let response = proxy.handle_license(request).await.unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(response.body, b"cba");
    assert_eq!(response.content_type, "application/xprotobuf");
}

#[tokio::test]
async fn platform_get_device_info() {
    let mut harness = setup().await;
    let client = &mut harness.client;
    send(
        client,
        ns::CONNECTION,
        "receiver-0",
        r#"{"type":"CONNECT"}"#,
    )
    .await;
    send(
        client,
        ns::DISCOVERY,
        "receiver-0",
        r#"{"type":"GET_DEVICE_INFO","requestId":3}"#,
    )
    .await;
    let reply = next_json(client).await;
    assert_eq!(reply["type"], "DEVICE_INFO");
    assert_eq!(reply["friendlyName"], "Living Room");
    assert_eq!(reply["deviceId"], "dev-1");
}

#[tokio::test]
async fn platform_setup_returns_eureka_info() {
    let mut harness = setup().await;
    let client = &mut harness.client;
    send(
        client,
        ns::CONNECTION,
        "receiver-0",
        r#"{"type":"CONNECT"}"#,
    )
    .await;
    send(
        client,
        ns::SETUP,
        "receiver-0",
        r#"{"type":"eureka_info","requestId":8}"#,
    )
    .await;
    let reply = next_json(client).await;
    assert_eq!(reply["type"], "eureka_info");
    assert_eq!(reply["response_code"], 200);
    assert_eq!(reply["data"]["device_info"]["ssdp_udn"], "dev-1");
}

#[tokio::test]
async fn platform_app_availability_marks_registered_app() {
    let mut harness = setup().await;
    let client = &mut harness.client;
    send(
        client,
        ns::CONNECTION,
        "receiver-0",
        r#"{"type":"CONNECT"}"#,
    )
    .await;
    send(
        client,
        ns::RECEIVER,
        "receiver-0",
        r#"{"type":"GET_APP_AVAILABILITY","requestId":4,"appId":["APP1"]}"#,
    )
    .await;
    let reply = next_json(client).await;
    assert_eq!(reply["type"], "GET_APP_AVAILABILITY");
    assert_eq!(reply["availability"]["APP1"], "APP_AVAILABLE");
}

#[tokio::test]
async fn platform_set_volume_broadcasts_receiver_status() {
    let mut harness = setup().await;
    let client = &mut harness.client;
    send(
        client,
        ns::CONNECTION,
        "receiver-0",
        r#"{"type":"CONNECT"}"#,
    )
    .await;
    send(
        client,
        ns::RECEIVER,
        "receiver-0",
        r#"{"type":"SET_VOLUME","requestId":6,"volume":{"level":0.5}}"#,
    )
    .await;
    let reply = next_json(client).await;
    assert_eq!(reply["type"], "RECEIVER_STATUS");
    assert_eq!(reply["status"]["volume"]["level"], 0.5);
    // muted was omitted, so it stays at its prior value.
    assert_eq!(reply["status"]["volume"]["muted"], false);
}
