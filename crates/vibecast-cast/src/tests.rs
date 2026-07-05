//! Connection-logic tests over in-memory duplex streams, plus a real TLS roundtrip.

use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use prost::Message as _;
use tokio::sync::mpsc;
use tokio_util::codec::Framed;

use vibecast_proto::{
    AuthChallenge, CastCodec, DeviceAuthMessage, HashAlgorithm, SignatureAlgorithm,
};
use vibecast_security::{AuthErrorType, CertificateBundle};

use crate::{message, namespace as ns};
use crate::{AuthMaterial, CastServer, ConnectionHandle, ServerEvent};

fn dummy_bundle() -> CertificateBundle {
    CertificateBundle {
        peer_cert_pem: Vec::new(),
        peer_key_pem: Vec::new(),
        peer_cert_der: vec![1, 2, 3],
        device_cert_der: vec![10, 11, 12],
        intermediate_certs_der: vec![vec![20, 21]],
        signature_sha1: vec![0xA1; 4],
        signature_sha256: vec![0xA2; 4],
        not_valid_before: 0,
        not_valid_after: i64::MAX,
        crl: None,
    }
}

fn dummy_auth() -> AuthMaterial {
    AuthMaterial {
        bundle: dummy_bundle(),
        crl: None,
    }
}

type DuplexClient = Framed<tokio::io::DuplexStream, CastCodec>;

/// Spawn a connection over a duplex pair; return the client framing, the event
/// receiver, and the connection handle from the Connected event.
async fn connect(
    auth: AuthMaterial,
) -> (DuplexClient, mpsc::Receiver<ServerEvent>, ConnectionHandle) {
    let (server_end, client_end) = tokio::io::duplex(64 * 1024);
    let (events_tx, mut events_rx) = mpsc::channel(16);
    tokio::spawn(crate::run_connection(
        server_end,
        1,
        Arc::from("test-peer"),
        Arc::new(auth),
        events_tx,
    ));
    let client = Framed::new(client_end, CastCodec);
    let handle = match events_rx.recv().await.unwrap() {
        ServerEvent::Connected(handle) => handle,
        other => panic!("expected Connected, got {other:?}"),
    };
    (client, events_rx, handle)
}

fn challenge(hash: HashAlgorithm, sig: SignatureAlgorithm) -> DeviceAuthMessage {
    DeviceAuthMessage {
        challenge: Some(AuthChallenge {
            signature_algorithm: Some(sig as i32),
            sender_nonce: None,
            hash_algorithm: Some(hash as i32),
        }),
        response: None,
        error: None,
    }
}

#[tokio::test]
async fn device_auth_challenge_returns_matching_static_signature() {
    let (mut client, _events, _handle) = connect(dummy_auth()).await;

    let payload =
        challenge(HashAlgorithm::Sha256, SignatureAlgorithm::RsassaPkcs1v15).encode_to_vec();
    client
        .send(message::build_binary(
            "sender-0",
            "receiver-0",
            ns::DEVICE_AUTH,
            payload,
        ))
        .await
        .unwrap();

    let reply = client.next().await.unwrap().unwrap();
    assert_eq!(reply.namespace, ns::DEVICE_AUTH);
    // Source/destination are swapped in the reply.
    assert_eq!(reply.source_id, "receiver-0");
    assert_eq!(reply.destination_id, "sender-0");

    let response = DeviceAuthMessage::decode(reply.payload_binary.as_deref().unwrap())
        .unwrap()
        .response
        .unwrap();
    assert_eq!(response.signature, vec![0xA2; 4]); // SHA-256 signature
    assert_eq!(response.client_auth_certificate, vec![10, 11, 12]);
    assert_eq!(response.hash_algorithm, Some(HashAlgorithm::Sha256 as i32));
}

#[tokio::test]
async fn unsupported_signature_algorithm_returns_auth_error() {
    let (mut client, _events, _handle) = connect(dummy_auth()).await;

    let payload = challenge(HashAlgorithm::Sha1, SignatureAlgorithm::RsassaPss).encode_to_vec();
    client
        .send(message::build_binary("s", "r", ns::DEVICE_AUTH, payload))
        .await
        .unwrap();

    let reply = client.next().await.unwrap().unwrap();
    let message = DeviceAuthMessage::decode(reply.payload_binary.as_deref().unwrap()).unwrap();
    assert!(message.response.is_none());
    assert_eq!(
        message.error.unwrap().error_type,
        AuthErrorType::SignatureAlgorithmUnavailable as i32
    );
}

#[tokio::test]
async fn heartbeat_ping_gets_pong() {
    let (mut client, _events, _handle) = connect(dummy_auth()).await;

    let ping = message::build_string(
        "sender-0",
        "receiver-0",
        ns::HEARTBEAT,
        r#"{"type":"PING"}"#.into(),
    );
    client.send(ping).await.unwrap();

    let reply = client.next().await.unwrap().unwrap();
    assert_eq!(reply.namespace, ns::HEARTBEAT);
    assert_eq!(reply.payload_utf8.as_deref(), Some(r#"{"type":"PONG"}"#));
}

#[tokio::test]
async fn non_local_messages_are_forwarded_as_events() {
    let (mut client, mut events, _handle) = connect(dummy_auth()).await;

    let msg = message::build_string(
        "sender-0",
        "receiver-0",
        ns::RECEIVER,
        r#"{"type":"GET_STATUS"}"#.into(),
    );
    client.send(msg).await.unwrap();

    match events.recv().await.unwrap() {
        ServerEvent::Message { message, handle } => {
            assert_eq!(message.namespace, ns::RECEIVER);
            assert_eq!(handle.peer(), "test-peer");
        }
        other => panic!("expected Message, got {other:?}"),
    }
}

#[tokio::test]
async fn dropping_client_emits_disconnected() {
    let (client, mut events, _handle) = connect(dummy_auth()).await;
    drop(client);

    match events.recv().await.unwrap() {
        ServerEvent::Disconnected { id, peer } => {
            assert_eq!(id, 1);
            assert_eq!(&*peer, "test-peer");
        }
        other => panic!("expected Disconnected, got {other:?}"),
    }
}

// --- TLS roundtrip --------------------------------------------------------

fn real_auth_material() -> (AuthMaterial, Vec<u8>) {
    let key = rcgen::KeyPair::generate().unwrap();
    let mut params = rcgen::CertificateParams::new(vec!["localhost".to_string()]).unwrap();
    params.not_before = rcgen::date_time_ymd(2000, 1, 1);
    params.not_after = rcgen::date_time_ymd(2099, 1, 1);
    let certificate = params.self_signed(&key).unwrap();
    let signature_sha256 = vec![0xC2; 6];

    let bundle = CertificateBundle {
        peer_cert_pem: certificate.pem().into_bytes(),
        peer_key_pem: key.serialize_pem().into_bytes(),
        peer_cert_der: certificate.der().to_vec(),
        device_cert_der: vec![10, 11, 12],
        intermediate_certs_der: vec![vec![20, 21]],
        signature_sha1: vec![0xC1; 5],
        signature_sha256: signature_sha256.clone(),
        not_valid_before: 0,
        not_valid_after: i64::MAX,
        crl: None,
    };
    (AuthMaterial { bundle, crl: None }, signature_sha256)
}

#[derive(Debug)]
struct NoVerify(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls_pki_types::CertificateDer<'_>,
        _intermediates: &[rustls_pki_types::CertificateDer<'_>],
        _server_name: &rustls_pki_types::ServerName<'_>,
        _ocsp: &[u8],
        _now: rustls_pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls_pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls_pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

fn client_config() -> rustls::ClientConfig {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .unwrap()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(NoVerify(provider)))
        .with_no_client_auth()
}

#[tokio::test]
async fn tls_handshake_then_device_auth_roundtrip() {
    let (auth, expected_sig) = real_auth_material();
    let resolver = vibecast_security::CertResolver::new(&auth.bundle).unwrap();
    let config = vibecast_security::server_config(resolver).unwrap();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (events_tx, mut events_rx) = mpsc::channel(16);
    let server = Arc::new(CastServer::new(config, auth, events_tx));
    let serve = {
        let server = server.clone();
        tokio::spawn(async move {
            let _ = server.serve(listener).await;
        })
    };

    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(client_config()));
    let server_name = rustls_pki_types::ServerName::try_from("localhost").unwrap();
    let tls = connector.connect(server_name, stream).await.unwrap();
    let mut client = Framed::new(tls, CastCodec);

    assert!(matches!(
        events_rx.recv().await.unwrap(),
        ServerEvent::Connected(_)
    ));

    let payload =
        challenge(HashAlgorithm::Sha256, SignatureAlgorithm::RsassaPkcs1v15).encode_to_vec();
    client
        .send(message::build_binary(
            "sender-0",
            "receiver-0",
            ns::DEVICE_AUTH,
            payload,
        ))
        .await
        .unwrap();

    let reply = client.next().await.unwrap().unwrap();
    let response = DeviceAuthMessage::decode(reply.payload_binary.as_deref().unwrap())
        .unwrap()
        .response
        .unwrap();
    assert_eq!(response.signature, expected_sig);

    serve.abort();
}
