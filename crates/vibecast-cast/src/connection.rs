//! Per-sender connection handling.
//!
//! Each connection runs a read loop plus a writer task fed by an mpsc channel
//! (the split-and-channel actor pattern), so the device hub can push messages to
//! a sender concurrently with the read loop. Device-auth and heartbeat are
//! answered locally; every other message is forwarded as a [`ServerEvent`].

use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use prost::Message as _;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;
use tokio_util::codec::Framed;

use vibecast_proto::cast_channel::cast_message::{PayloadType, ProtocolVersion};
use vibecast_proto::{
    CastCodec, CastMessage, DeviceAuthMessage, FramingError, HashAlgorithm, SignatureAlgorithm,
};
use vibecast_security::{build_auth_error, build_auth_response, AuthErrorType, CertificateBundle};

use crate::error::CastError;
use crate::{message, namespace as ns};

/// Device-auth material captured for the lifetime of a connection.
#[derive(Clone, Debug)]
pub struct AuthMaterial {
    /// The active certificate bundle (device cert, intermediates, signatures).
    pub bundle: CertificateBundle,
    /// Server-level CRL override; when `None`, the bundle's own CRL is used.
    pub crl: Option<Vec<u8>>,
}

/// A cloneable handle for sending messages to one connected sender.
#[derive(Clone, Debug)]
pub struct ConnectionHandle {
    id: u64,
    peer: Arc<str>,
    tx: mpsc::Sender<CastMessage>,
}

impl ConnectionHandle {
    /// Stable, unique connection id.
    #[must_use]
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Best-effort peer address, for logging.
    #[must_use]
    pub fn peer(&self) -> &str {
        &self.peer
    }

    /// Send a pre-built message to this sender.
    pub async fn send(&self, message: CastMessage) -> Result<(), CastError> {
        self.tx.send(message).await.map_err(|_| CastError::Closed)
    }

    /// Send a JSON (STRING) message (compact, no whitespace).
    pub async fn send_json(
        &self,
        source_id: &str,
        dest_id: &str,
        namespace: &str,
        data: &serde_json::Value,
    ) -> Result<(), CastError> {
        let payload = serde_json::to_string(data)?;
        self.send(message::build_string(
            source_id, dest_id, namespace, payload,
        ))
        .await
    }

    /// Send a BINARY message.
    pub async fn send_binary(
        &self,
        source_id: &str,
        dest_id: &str,
        namespace: &str,
        data: Vec<u8>,
    ) -> Result<(), CastError> {
        self.send(message::build_binary(source_id, dest_id, namespace, data))
            .await
    }
}

/// Lifecycle and message events emitted by connections to the server consumer.
#[derive(Debug)]
pub enum ServerEvent {
    /// A sender connected (after TLS handshake).
    Connected(ConnectionHandle),
    /// A non-local message was received from a sender.
    Message {
        /// Handle to reply to the originating sender.
        handle: ConnectionHandle,
        /// The received message.
        message: CastMessage,
    },
    /// A sender disconnected.
    Disconnected {
        /// The connection id that closed.
        id: u64,
        /// The peer address that closed.
        peer: Arc<str>,
    },
}

/// Aborts the wrapped task when dropped, so the writer never outlives the reader.
struct AbortGuard(tokio::task::JoinHandle<()>);

impl Drop for AbortGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Run one connection to completion over `stream`.
///
/// Returns when the peer disconnects or a fatal framing error occurs. Emits
/// [`ServerEvent::Connected`] on entry and [`ServerEvent::Disconnected`] on exit.
pub async fn run_connection<S>(
    stream: S,
    id: u64,
    peer: Arc<str>,
    auth: Arc<AuthMaterial>,
    events: mpsc::Sender<ServerEvent>,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut sink, mut reader) = Framed::new(stream, CastCodec).split();
    let (tx, mut rx) = mpsc::channel::<CastMessage>(64);
    let handle = ConnectionHandle {
        id,
        peer: Arc::clone(&peer),
        tx,
    };

    // Writer task: drains outbound messages onto the wire. Aborted on drop.
    let _writer = AbortGuard(tokio::spawn(async move {
        while let Some(message) = rx.recv().await {
            if sink.send(message).await.is_err() {
                break;
            }
        }
    }));

    tracing::info!(peer = %peer, "connection opened");
    if events
        .send(ServerEvent::Connected(handle.clone()))
        .await
        .is_err()
    {
        return;
    }

    loop {
        match reader.next().await {
            Some(Ok(message)) => {
                if !dispatch(message, &auth, &handle, &events).await {
                    break;
                }
            }
            Some(Err(FramingError::Io(_))) => break, // transport disconnect
            Some(Err(err)) => {
                tracing::warn!(peer = %peer, error = %err, "framing error, closing");
                break;
            }
            None => break, // clean EOF
        }
    }

    tracing::info!(peer = %peer, "connection closed");
    let _ = events.send(ServerEvent::Disconnected { id, peer }).await;
}

/// Route one message. Returns `false` if the connection should shut down.
async fn dispatch(
    message: CastMessage,
    auth: &AuthMaterial,
    handle: &ConnectionHandle,
    events: &mpsc::Sender<ServerEvent>,
) -> bool {
    if message.protocol_version != ProtocolVersion::Castv210 as i32 {
        tracing::warn!(
            peer = %handle.peer,
            version = message.protocol_version,
            "unexpected protocol version"
        );
    }

    match message.namespace.as_str() {
        ns::DEVICE_AUTH => {
            let Some(payload) = device_auth_response(&message, auth) else {
                return true;
            };
            handle
                .send_binary(
                    &message.destination_id,
                    &message.source_id,
                    ns::DEVICE_AUTH,
                    payload,
                )
                .await
                .is_ok()
        }
        ns::HEARTBEAT => {
            if is_ping(&message) {
                let pong = serde_json::json!({ "type": "PONG" });
                return handle
                    .send_json(
                        &message.destination_id,
                        &message.source_id,
                        ns::HEARTBEAT,
                        &pong,
                    )
                    .await
                    .is_ok();
            }
            true
        }
        _ => events
            .send(ServerEvent::Message {
                handle: handle.clone(),
                message,
            })
            .await
            .is_ok(),
    }
}

/// Build the deviceauth reply for a challenge, or `None` if it can't be parsed.
fn device_auth_response(message: &CastMessage, auth: &AuthMaterial) -> Option<Vec<u8>> {
    let payload = message.payload_binary.as_deref().unwrap_or(&[]);
    let challenge = match DeviceAuthMessage::decode(payload) {
        Ok(challenge) => challenge,
        Err(_) => {
            tracing::warn!("failed to parse device auth challenge");
            return None;
        }
    };

    // proto2 defaults: hash=SHA1, signature=RSASSA_PKCS1v15 when unset.
    let (requested_hash, requested_sig) = match &challenge.challenge {
        Some(inner) => (
            inner.hash_algorithm.unwrap_or(HashAlgorithm::Sha1 as i32),
            inner
                .signature_algorithm
                .unwrap_or(SignatureAlgorithm::RsassaPkcs1v15 as i32),
        ),
        None => (
            HashAlgorithm::Sha1 as i32,
            SignatureAlgorithm::RsassaPkcs1v15 as i32,
        ),
    };

    if requested_sig != SignatureAlgorithm::RsassaPkcs1v15 as i32 {
        return Some(build_auth_error(
            AuthErrorType::SignatureAlgorithmUnavailable,
        ));
    }

    match HashAlgorithm::try_from(requested_hash) {
        Ok(hash) => Some(build_auth_response(&auth.bundle, hash, auth.crl.as_deref())),
        Err(_) => Some(build_auth_error(
            AuthErrorType::SignatureAlgorithmUnavailable,
        )),
    }
}

fn is_ping(message: &CastMessage) -> bool {
    message.payload_type == PayloadType::String as i32
        && message
            .payload_utf8
            .as_deref()
            .is_some_and(|payload| payload.contains("\"PING\""))
}
