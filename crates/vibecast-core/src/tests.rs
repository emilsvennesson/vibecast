//! Integration tests: transport + hub over an in-memory duplex stream.

use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::io::DuplexStream;
use tokio::sync::mpsc;
use tokio_util::codec::Framed;

use vibecast_cast::{message, namespace as ns, run_connection, AuthMaterial};
use vibecast_proto::CastCodec;
use vibecast_security::CertificateBundle;

use crate::{DeviceHub, DeviceIdentity};

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

/// Wire a real connection to a hub over a duplex; return the client framing.
async fn setup() -> Framed<DuplexStream, CastCodec> {
    let (server_end, client_end) = tokio::io::duplex(64 * 1024);
    let (events_tx, events_rx) = mpsc::channel(32);
    tokio::spawn(run_connection(
        server_end,
        1,
        Arc::from("peer"),
        Arc::new(dummy_auth()),
        events_tx,
    ));
    let hub = DeviceHub::new(DeviceIdentity::new(
        "Living Room".into(),
        "Chromecast".into(),
        "dev-1234".into(),
    ));
    tokio::spawn(hub.run(events_rx));
    Framed::new(client_end, CastCodec)
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

async fn connect(client: &mut Framed<DuplexStream, CastCodec>) {
    send(
        client,
        ns::CONNECTION,
        "receiver-0",
        r#"{"type":"CONNECT"}"#,
    )
    .await;
}

#[tokio::test]
async fn get_status_returns_receiver_status() {
    let mut client = setup().await;
    connect(&mut client).await;
    send(
        &mut client,
        ns::RECEIVER,
        "receiver-0",
        r#"{"type":"GET_STATUS","requestId":1}"#,
    )
    .await;

    let reply = next_json(&mut client).await;
    assert_eq!(reply["type"], "RECEIVER_STATUS");
    assert_eq!(reply["requestId"], 1);
    assert_eq!(reply["status"]["applications"], serde_json::json!([]));
    assert_eq!(reply["status"]["volume"]["controlType"], "attenuation");
    assert_eq!(reply["status"]["isActiveInput"], true);
}

#[tokio::test]
async fn set_volume_broadcasts_updated_status() {
    let mut client = setup().await;
    connect(&mut client).await;
    send(
        &mut client,
        ns::RECEIVER,
        "receiver-0",
        r#"{"type":"SET_VOLUME","requestId":2,"volume":{"level":0.5}}"#,
    )
    .await;

    let reply = next_json(&mut client).await;
    assert_eq!(reply["type"], "RECEIVER_STATUS");
    assert_eq!(reply["status"]["volume"]["level"], 0.5);
    // muted was not provided, so it stays at the default false.
    assert_eq!(reply["status"]["volume"]["muted"], false);
}

#[tokio::test]
async fn get_app_availability_marks_available() {
    let mut client = setup().await;
    connect(&mut client).await;
    send(
        &mut client,
        ns::RECEIVER,
        "receiver-0",
        r#"{"type":"GET_APP_AVAILABILITY","requestId":4,"appId":["95370A1C"]}"#,
    )
    .await;

    let reply = next_json(&mut client).await;
    assert_eq!(reply["type"], "GET_APP_AVAILABILITY");
    assert_eq!(reply["availability"]["95370A1C"], "APP_AVAILABLE");
}

#[tokio::test]
async fn launch_without_apps_returns_error() {
    let mut client = setup().await;
    connect(&mut client).await;
    send(
        &mut client,
        ns::RECEIVER,
        "receiver-0",
        r#"{"type":"LAUNCH","requestId":5,"appId":"95370A1C"}"#,
    )
    .await;

    let reply = next_json(&mut client).await;
    assert_eq!(reply["type"], "LAUNCH_ERROR");
    assert_eq!(reply["requestId"], 5);
}

#[tokio::test]
async fn invalid_receiver_request_returns_invalid_request() {
    let mut client = setup().await;
    connect(&mut client).await;
    send(
        &mut client,
        ns::RECEIVER,
        "receiver-0",
        r#"{"type":"BOGUS","requestId":6}"#,
    )
    .await;

    let reply = next_json(&mut client).await;
    assert_eq!(reply["type"], "INVALID_REQUEST");
    assert_eq!(reply["requestId"], 6);
}

#[tokio::test]
async fn get_device_info_returns_device_metadata() {
    let mut client = setup().await;
    connect(&mut client).await;
    send(
        &mut client,
        ns::DISCOVERY,
        "receiver-0",
        r#"{"type":"GET_DEVICE_INFO","requestId":3}"#,
    )
    .await;

    let reply = next_json(&mut client).await;
    assert_eq!(reply["type"], "DEVICE_INFO");
    assert_eq!(reply["friendlyName"], "Living Room");
    assert_eq!(reply["deviceModel"], "Chromecast");
    assert_eq!(reply["deviceId"], "dev-1234");
}

#[tokio::test]
async fn setup_request_returns_eureka_info() {
    let mut client = setup().await;
    connect(&mut client).await;
    send(
        &mut client,
        ns::SETUP,
        "receiver-0",
        r#"{"type":"eureka_info","requestId":8}"#,
    )
    .await;

    let reply = next_json(&mut client).await;
    assert_eq!(reply["type"], "eureka_info");
    assert_eq!(reply["response_code"], 200);
    assert_eq!(reply["data"]["name"], "Living Room");
    assert_eq!(reply["data"]["device_info"]["ssdp_udn"], "dev-1234");
}
