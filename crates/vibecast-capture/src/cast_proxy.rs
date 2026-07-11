//! CastV2 MITM proxy (sender ↔ proxy ↔ genuine receiver).
//!
//! Presents the harvested identity to the sender, relays every framed
//! `CastMessage` to/from the real receiver, and logs all of it. The
//! `deviceauth` challenge is answered locally (the harvested signature binds
//! *our* peer certificate, so the real receiver's response cannot be
//! forwarded); everything else is passed through verbatim.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use prost::Message as _;
use serde_json::{Map, Value};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_rustls::{TlsAcceptor, TlsConnector};
use tokio_util::codec::Framed;

use vibecast_proto::cast_channel::cast_message::{PayloadType, ProtocolVersion};
use vibecast_proto::{
    CastCodec, CastMessage, DeviceAuthMessage, HashAlgorithm, SignatureAlgorithm,
};
use vibecast_security::{build_auth_error, build_auth_response, AuthErrorType, CertificateBundle};

use crate::decode::{payload_type_name, payload_value, DEVICE_AUTH};
use crate::recorder::Recorder;
use crate::tls::upstream_client_config;

/// A running Cast MITM proxy. Cheap to `Arc`-clone per connection.
pub struct CastProxy {
    recorder: Arc<Recorder>,
    acceptor: TlsAcceptor,
    connector: TlsConnector,
    bundle: CertificateBundle,
    crl: Option<Vec<u8>>,
    upstream_host: String,
    upstream_port: u16,
    conn_counter: AtomicU64,
}

impl CastProxy {
    /// Build a proxy from the sender-facing TLS config + harvested bundle.
    pub fn new(
        recorder: Arc<Recorder>,
        server_config: rustls::ServerConfig,
        bundle: CertificateBundle,
        crl: Option<Vec<u8>>,
        upstream_host: String,
        upstream_port: u16,
    ) -> Self {
        Self {
            recorder,
            acceptor: TlsAcceptor::from(Arc::new(server_config)),
            connector: TlsConnector::from(Arc::new(upstream_client_config())),
            bundle,
            crl,
            upstream_host,
            upstream_port,
            conn_counter: AtomicU64::new(1),
        }
    }

    /// Accept sender connections until `listener` errors (e.g. on shutdown).
    pub async fn serve(self: Arc<Self>, listener: TcpListener) {
        loop {
            let (stream, peer) = match listener.accept().await {
                Ok(pair) => pair,
                Err(error) => {
                    tracing::debug!(%error, "cast listener stopped accepting");
                    return;
                }
            };
            let me = Arc::clone(&self);
            tokio::spawn(async move {
                let id = me.conn_counter.fetch_add(1, Ordering::Relaxed);
                if let Err(error) = me.handle(stream, peer.to_string(), id).await {
                    tracing::debug!(connection = id, %error, "cast connection ended");
                }
            });
        }
    }

    async fn handle(
        self: Arc<Self>,
        stream: TcpStream,
        peer: String,
        id: u64,
    ) -> std::io::Result<()> {
        self.recorder.meta(
            "sender_connected",
            fields([("connection_id", id.into()), ("peer", peer.into())]),
        );

        let sender_tls = self.acceptor.accept(stream).await?;

        let upstream_tcp =
            TcpStream::connect((self.upstream_host.as_str(), self.upstream_port)).await?;
        let server_name = rustls_pki_types::ServerName::try_from(self.upstream_host.clone())
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e.to_string()))?;
        let upstream_tls = self.connector.connect(server_name, upstream_tcp).await?;
        self.recorder.meta(
            "upstream_connected",
            fields([
                ("connection_id", id.into()),
                (
                    "upstream",
                    format!("{}:{}", self.upstream_host, self.upstream_port).into(),
                ),
            ]),
        );

        let (mut sender_sink, mut sender_read) = Framed::new(sender_tls, CastCodec).split();
        let (mut upstream_sink, mut upstream_read) = Framed::new(upstream_tls, CastCodec).split();

        // Single writer task owns the sender sink; both directions push to it.
        let (sender_tx, mut sender_rx) = mpsc::channel::<CastMessage>(64);
        let writer = tokio::spawn(async move {
            while let Some(msg) = sender_rx.recv().await {
                if sender_sink.send(msg).await.is_err() {
                    break;
                }
            }
        });

        // upstream -> sender
        let u2s = {
            let recorder = Arc::clone(&self.recorder);
            let tx = sender_tx.clone();
            tokio::spawn(async move {
                while let Some(item) = upstream_read.next().await {
                    let Ok(msg) = item else { break };
                    recorder.cast(cast_entry(id, "device_to_sender", &msg));
                    if tx.send(msg).await.is_err() {
                        break;
                    }
                }
            })
        };

        // sender -> upstream (device auth answered locally)
        while let Some(item) = sender_read.next().await {
            let Ok(msg) = item else { break };

            if msg.namespace == DEVICE_AUTH {
                self.recorder.cast(cast_entry(id, "sender_to_proxy", &msg));
                let reply = self.device_auth_reply(&msg);
                self.recorder
                    .cast(cast_entry(id, "proxy_to_sender", &reply));
                if sender_tx.send(reply).await.is_err() {
                    break;
                }
                continue;
            }

            self.recorder.cast(cast_entry(id, "sender_to_device", &msg));
            if upstream_sink.send(msg).await.is_err() {
                break;
            }
        }

        // Tear the connection down.
        drop(sender_tx);
        u2s.abort();
        writer.abort();
        self.recorder.meta(
            "sender_disconnected",
            fields([("connection_id", id.into())]),
        );
        Ok(())
    }

    /// Build the local `deviceauth` reply for a sender challenge.
    fn device_auth_reply(&self, challenge: &CastMessage) -> CastMessage {
        let payload = self.device_auth_payload(challenge);
        build_binary(
            &challenge.destination_id,
            &challenge.source_id,
            DEVICE_AUTH,
            payload,
        )
    }

    fn device_auth_payload(&self, message: &CastMessage) -> Vec<u8> {
        let payload = message.payload_binary.as_deref().unwrap_or_default();
        // proto2 defaults: hash=SHA1, signature=RSASSA_PKCS1v15 when unset.
        let (hash, sig) = match DeviceAuthMessage::decode(payload) {
            Ok(DeviceAuthMessage {
                challenge: Some(ch),
                ..
            }) => (
                ch.hash_algorithm.unwrap_or(HashAlgorithm::Sha1 as i32),
                ch.signature_algorithm
                    .unwrap_or(SignatureAlgorithm::RsassaPkcs1v15 as i32),
            ),
            _ => (
                HashAlgorithm::Sha1 as i32,
                SignatureAlgorithm::RsassaPkcs1v15 as i32,
            ),
        };

        if sig != SignatureAlgorithm::RsassaPkcs1v15 as i32 {
            return build_auth_error(AuthErrorType::SignatureAlgorithmUnavailable);
        }
        match HashAlgorithm::try_from(hash) {
            Ok(hash) => build_auth_response(&self.bundle, hash, self.crl.as_deref()),
            Err(_) => build_auth_error(AuthErrorType::SignatureAlgorithmUnavailable),
        }
    }
}

fn cast_entry(id: u64, direction: &str, msg: &CastMessage) -> Map<String, Value> {
    fields([
        ("layer", "cast".into()),
        ("direction", direction.into()),
        ("connection_id", id.into()),
        ("source_id", msg.source_id.clone().into()),
        ("destination_id", msg.destination_id.clone().into()),
        ("namespace", msg.namespace.clone().into()),
        ("payload_type", payload_type_name(msg).into()),
        ("payload", payload_value(msg)),
    ])
}

fn fields<const N: usize>(pairs: [(&str, Value); N]) -> Map<String, Value> {
    pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()
}

fn build_binary(source_id: &str, dest_id: &str, namespace: &str, payload: Vec<u8>) -> CastMessage {
    CastMessage {
        protocol_version: ProtocolVersion::Castv210 as i32,
        source_id: source_id.to_owned(),
        destination_id: dest_id.to_owned(),
        namespace: namespace.to_owned(),
        payload_type: PayloadType::Binary as i32,
        payload_utf8: None,
        payload_binary: Some(payload),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use tokio::io::{AsyncRead, AsyncWrite};
    use tokio::net::TcpListener;
    use vibecast_proto::{AuthChallenge, DeviceAuthMessage};
    use vibecast_security::{server_config, CertResolver};

    const RECEIVER: &str = "urn:x-cast:com.google.cast.receiver";

    fn test_bundle() -> CertificateBundle {
        let key = rcgen::KeyPair::generate().unwrap();
        let params = rcgen::CertificateParams::new(vec!["localhost".to_string()]).unwrap();
        let cert = params.self_signed(&key).unwrap();
        CertificateBundle {
            peer_cert_pem: cert.pem().into_bytes(),
            peer_key_pem: key.serialize_pem().into_bytes(),
            peer_cert_der: cert.der().to_vec(),
            device_cert_der: vec![10, 11, 12],
            intermediate_certs_der: vec![vec![20, 21]],
            signature_sha1: vec![0xA1; 8],
            signature_sha256: vec![0xA2; 8],
            not_valid_before: 0,
            not_valid_after: i64::MAX,
            crl: None,
        }
    }

    fn framed<S>(stream: S) -> Framed<S, CastCodec>
    where
        S: AsyncRead + AsyncWrite,
    {
        Framed::new(stream, CastCodec)
    }

    fn build_string(src: &str, dst: &str, namespace: &str, payload: String) -> CastMessage {
        CastMessage {
            protocol_version: ProtocolVersion::Castv210 as i32,
            source_id: src.to_owned(),
            destination_id: dst.to_owned(),
            namespace: namespace.to_owned(),
            payload_type: PayloadType::String as i32,
            payload_utf8: Some(payload),
            payload_binary: None,
        }
    }

    fn challenge_payload() -> Vec<u8> {
        DeviceAuthMessage {
            challenge: Some(AuthChallenge {
                signature_algorithm: Some(SignatureAlgorithm::RsassaPkcs1v15 as i32),
                sender_nonce: None,
                hash_algorithm: Some(HashAlgorithm::Sha256 as i32),
            }),
            response: None,
            error: None,
        }
        .encode_to_vec()
    }

    /// A stub "genuine receiver": accepts one TLS connection, reads one message
    /// and echoes back a RECEIVER_STATUS so forwarding can be observed.
    async fn spawn_stub_upstream(bundle: &CertificateBundle) -> u16 {
        let resolver = CertResolver::new(bundle).unwrap();
        let config = server_config(resolver).unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let tls = acceptor.accept(stream).await.unwrap();
            let mut conn = framed(tls);
            // Read the forwarded GET_STATUS, then reply.
            let msg = conn.next().await.unwrap().unwrap();
            assert_eq!(msg.namespace, RECEIVER);
            conn.send(build_string(
                "receiver-0",
                "sender-0",
                RECEIVER,
                r#"{"type":"RECEIVER_STATUS","status":{"applications":[]}}"#.into(),
            ))
            .await
            .unwrap();
            // Keep the connection open briefly.
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        });
        port
    }

    #[tokio::test]
    async fn answers_device_auth_locally_and_forwards_rest() {
        let bundle = test_bundle();
        let upstream_port = spawn_stub_upstream(&bundle).await;

        let dir = std::env::temp_dir().join(format!(
            "vibecast-capture-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let recorder = Arc::new(Recorder::create(&dir).unwrap());

        let resolver = CertResolver::new(&bundle).unwrap();
        let sender_config = server_config(resolver).unwrap();
        let proxy = Arc::new(CastProxy::new(
            Arc::clone(&recorder),
            sender_config,
            bundle.clone(),
            None,
            "127.0.0.1".to_string(),
            upstream_port,
        ));

        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let proxy_addr = listener.local_addr().unwrap();
        let serve = tokio::spawn(Arc::clone(&proxy).serve(listener));

        // Connect as a sender.
        let tcp = tokio::net::TcpStream::connect(proxy_addr).await.unwrap();
        let connector = TlsConnector::from(Arc::new(upstream_client_config()));
        let name = rustls_pki_types::ServerName::try_from("localhost").unwrap();
        let tls = connector.connect(name, tcp).await.unwrap();
        let mut client = framed(tls);

        // Device auth is answered locally with the bundle's SHA-256 signature.
        client
            .send(build_binary(
                "sender-0",
                "receiver-0",
                DEVICE_AUTH,
                challenge_payload(),
            ))
            .await
            .unwrap();
        let reply = client.next().await.unwrap().unwrap();
        assert_eq!(reply.namespace, DEVICE_AUTH);
        let response = DeviceAuthMessage::decode(reply.payload_binary.as_deref().unwrap())
            .unwrap()
            .response
            .unwrap();
        assert_eq!(response.signature, vec![0xA2; 8]);

        // A RECEIVER message is forwarded upstream and its reply comes back.
        client
            .send(build_string(
                "sender-0",
                "receiver-0",
                RECEIVER,
                r#"{"type":"GET_STATUS"}"#.into(),
            ))
            .await
            .unwrap();
        let forwarded = client.next().await.unwrap().unwrap();
        assert_eq!(forwarded.namespace, RECEIVER);
        let payload: serde_json::Value =
            serde_json::from_str(forwarded.payload_utf8.as_deref().unwrap()).unwrap();
        assert_eq!(payload["type"], "RECEIVER_STATUS");

        serve.abort();

        // The capture log recorded cast messages.
        let log = std::fs::read_to_string(dir.join("cast.jsonl")).unwrap();
        assert!(log.contains("sender_to_proxy"));
        assert!(log.contains("proxy_to_sender"));
        assert!(log.contains("sender_to_device"));
        assert!(log.contains("device_to_sender"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
