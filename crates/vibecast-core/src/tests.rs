//! End-to-end tests: a real Cast connection + hub driving a fake app,
//! fake renderer, and fake proxy registrar over an in-memory duplex stream.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::io::DuplexStream;
use tokio::sync::mpsc;
use tokio_util::codec::Framed;

use vibecast_bridge::{LicenseHandler, ManifestHandler, PlayerCommand, PlayerReport, Renderer};
use vibecast_cast::{message, namespace as ns, run_connection, AuthMaterial, ServerEvent};
use vibecast_messages::{PlayerState, Volume};
use vibecast_proto::CastCodec;
use vibecast_sdk::{
    AppContext, AppProvider, AppSession, LaunchCredentials, LaunchError, LoadRequest,
    MediaResolveError, PlaybackMedia, PlaybackStream, StreamType,
};
use vibecast_security::CertificateBundle;

use crate::{AppRegistry, DeviceHub, DeviceIdentity, HubConfig, HubEvent, ProxyRegistrar};

// -- fakes -----------------------------------------------------------------

#[derive(Default)]
struct FakeRenderer {
    commands: Mutex<Vec<PlayerCommand>>,
}

#[async_trait]
impl Renderer for FakeRenderer {
    async fn send(&self, command: PlayerCommand) {
        self.commands.lock().unwrap().push(command);
    }
}

impl FakeRenderer {
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
    async fn launch(
        &self,
        _ctx: &AppContext,
        _credentials: LaunchCredentials,
    ) -> Result<Box<dyn AppSession>, LaunchError> {
        Ok(Box::new(FakeSession))
    }
}

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
            vec![PlaybackStream {
                url: "https://cdn.example/manifest.mpd".into(),
                content_type: "application/dash+xml".into(),
                drm: None,
            }],
            StreamType::Buffered,
        );
        media.title = Some("Fake Title".into());
        media.duration = Some(120.0);
        media.start_time = request.current_time;
        Ok(media)
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
    hub_tx: mpsc::Sender<HubEvent>,
    renderer: Arc<FakeRenderer>,
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

    let renderer = Arc::new(FakeRenderer::default());
    let proxy = Arc::new(FakeProxy::default());
    let hub = DeviceHub::new(HubConfig {
        identity: DeviceIdentity::new("Living Room".into(), "Chromecast".into(), "dev-1".into()),
        registry: AppRegistry::new(vec![Arc::new(FakeApp)]),
        renderer: renderer.clone(),
        proxy: proxy.clone(),
        http: reqwest::Client::new(),
        data_dir: std::env::temp_dir().join("vibecast-core-tests"),
        volume: attenuation_volume(),
    });
    let hub_tx = hub.sender();
    {
        let hub_tx = hub_tx.clone();
        tokio::spawn(async move {
            while let Some(event) = events_rx.recv().await {
                if hub_tx.send(HubEvent::Server(event)).await.is_err() {
                    break;
                }
            }
        });
    }
    tokio::spawn(hub.run());

    Harness {
        client: Framed::new(client_end, CastCodec),
        hub_tx,
        renderer,
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

    // The renderer received Load then Play; the manifest proxy was registered.
    let commands = harness.renderer.commands();
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

    // A player state report from the renderer bridge.
    harness
        .hub_tx
        .send(HubEvent::Report(PlayerReport::State {
            session_id: transport.clone(),
            player_state: PlayerState::Playing,
            current_time: 33.5,
            duration: Some(120.0),
            idle_reason: None,
        }))
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
