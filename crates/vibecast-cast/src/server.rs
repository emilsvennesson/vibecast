//! TLS listener that accepts Cast senders and spawns per-connection tasks.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use arc_swap::ArcSwap;
use rustls::ServerConfig;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_rustls::TlsAcceptor;

use crate::connection::{run_connection, AuthMaterial, ServerEvent};

/// Accepts TLS connections on a listener and drives each as a Cast connection.
///
/// Device-auth material is held behind an [`ArcSwap`] so it can be hot-reloaded
/// with [`CastServer::update_auth`]; connections snapshot it at accept time.
pub struct CastServer {
    acceptor: TlsAcceptor,
    auth: Arc<ArcSwap<AuthMaterial>>,
    events: mpsc::Sender<ServerEvent>,
    next_id: AtomicU64,
}

impl CastServer {
    /// Create a server from a rustls config, initial auth material, and an
    /// event sink that receives connection lifecycle and inbound messages.
    #[must_use]
    pub fn new(
        config: ServerConfig,
        auth: AuthMaterial,
        events: mpsc::Sender<ServerEvent>,
    ) -> Self {
        Self {
            acceptor: TlsAcceptor::from(Arc::new(config)),
            auth: Arc::new(ArcSwap::from_pointee(auth)),
            events,
            next_id: AtomicU64::new(1),
        }
    }

    /// Atomically replace the device-auth material for future connections.
    pub fn update_auth(&self, auth: AuthMaterial) {
        self.auth.store(Arc::new(auth));
    }

    /// Accept connections until the listener errors (e.g. is closed).
    ///
    /// Each accepted connection is handled in its own task; a slow or failed
    /// TLS handshake never blocks the accept loop.
    pub async fn serve(&self, listener: TcpListener) -> std::io::Result<()> {
        loop {
            let (stream, peer) = listener.accept().await?;
            let acceptor = self.acceptor.clone();
            let auth = self.auth.load_full();
            let events = self.events.clone();
            let id = self.next_id.fetch_add(1, Ordering::Relaxed);
            let peer: Arc<str> = Arc::from(peer.to_string());

            tokio::spawn(async move {
                match acceptor.accept(stream).await {
                    Ok(tls) => run_connection(tls, id, peer, auth, events).await,
                    Err(err) => tracing::warn!(peer = %peer, error = %err, "TLS handshake failed"),
                }
            });
        }
    }
}
