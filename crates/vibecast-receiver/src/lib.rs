//! Generic Cast receiver composition.
//!
//! [`spawn`] assembles and supervises **one** Cast receiver instance — the
//! device hub, the CastV2 TLS server, the eureka HTTP/HTTPS endpoints, and the
//! mDNS advertisement — bound to a single [`Player`] and app [`AppRegistry`].
//! It is deliberately independent of any particular app set, the player bridge,
//! or vibecast branding: the caller injects the identity, ports, certificate
//! material, app registry, player command sink, and proxy registrar.
//!
//! Multiple receivers can be spawned on one host (each with its own identity and
//! ports); certificate rotation is intentionally *not* owned here (the active
//! certificate is shared across receivers) — instead each receiver exposes a
//! [`RotationHandle`] so a single external rotation loop can hot-swap the
//! device-auth material and advertised digest of every receiver.

#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use vibecast_cast::{AuthMaterial, CastServer, ServerEvent};
use vibecast_core::{AppRegistry, DeviceHub, DeviceHubHandle, DeviceIdentity, HubConfig};
use vibecast_discovery::{CastAdvertisement, DiscoveryError, EurekaIdentity, EurekaServer};
use vibecast_messages::Volume;
use vibecast_player_api::{Player, PlayerReport, ProxyRegistrar};
use vibecast_sdk::PlayerCapabilities;
use vibecast_security::{server_config, CertResolver, CertificateBundle, SecurityError};

/// Everything needed to compose one receiver instance.
pub struct ReceiverParams {
    /// Cast device identity (friendly name, model, device id).
    pub identity: DeviceIdentity,
    /// Eureka `/setup/eureka_info` identity.
    pub eureka_identity: EurekaIdentity,
    /// App registry (which Cast apps this receiver serves).
    pub registry: AppRegistry,
    /// Player command sink for this receiver.
    pub player: Arc<dyn Player>,
    /// Session proxy registrar for this receiver.
    pub proxy: Arc<dyn ProxyRegistrar>,
    /// Player reports for this receiver (state/error from its player).
    pub reports: mpsc::Receiver<PlayerReport>,
    /// Shared HTTP client.
    pub http: reqwest::Client,
    /// Base data directory.
    pub data_dir: PathBuf,
    /// Initial receiver volume.
    pub volume: Volume,
    /// User-Agent for app sessions.
    pub user_agent: String,
    /// `CAST-DEVICE-CAPABILITIES` header value for app sessions.
    pub cast_device_capabilities: String,
    /// Capabilities of the player bound to this receiver.
    pub capabilities: PlayerCapabilities,
    /// Shared TLS cert resolver (rotation updates this once for all receivers).
    pub resolver: Arc<CertResolver>,
    /// Active certificate bundle (device-auth material + digest source).
    pub bundle: CertificateBundle,
    /// Device-auth CRL, if any.
    pub crl: Option<Vec<u8>>,
    /// Host to bind listeners on.
    pub bind_host: String,
    /// CastV2 TLS port (`0` binds an OS-assigned port).
    pub cast_port: u16,
    /// Eureka HTTP port (`0` binds an OS-assigned port).
    pub eureka_http_port: u16,
    /// Eureka HTTPS port (`0` binds an OS-assigned port).
    pub eureka_https_port: u16,
    /// Advertise over mDNS from Rust (desktop `true`; frontend-owned discovery `false`).
    pub advertise_mdns: bool,
    /// App ids advertised over mDNS (base + app subtypes).
    pub app_ids: Vec<String>,
}

/// Errors composing or starting a receiver.
#[derive(Debug, thiserror::Error)]
pub enum ReceiverError {
    /// Building the TLS server config failed.
    #[error("certificate/TLS setup failed")]
    Certs(#[from] SecurityError),
    /// Building the eureka server or starting mDNS failed.
    #[error("discovery setup failed")]
    Discovery(#[from] DiscoveryError),
    /// Binding a listening socket failed.
    #[error("binding {what} on port {port}")]
    Bind {
        /// Which listener failed to bind.
        what: &'static str,
        /// The requested port.
        port: u16,
        /// Underlying IO error.
        #[source]
        source: std::io::Error,
    },
}

/// A cheaply-cloneable handle for pushing certificate-rotation updates into a
/// running receiver from an external rotation loop.
#[derive(Clone)]
pub struct RotationHandle {
    cast_server: Arc<CastServer>,
    advertisement: Arc<tokio::sync::Mutex<CastAdvertisement>>,
}

impl RotationHandle {
    /// Hot-swap the device-auth material for future connections.
    pub fn update_auth(&self, auth: AuthMaterial) {
        self.cast_server.update_auth(auth);
    }

    /// Update the advertised certificate digest; returns the new TXT pairs.
    pub async fn update_cert_digest(&self, digest: &str) -> Vec<(String, String)> {
        let mut advertisement = self.advertisement.lock().await;
        if let Err(error) = advertisement.update_cert_digest(digest) {
            tracing::error!(%error, "failed to update advertised certificate digest");
        }
        txt_pairs(&advertisement)
    }
}

/// A running receiver instance and the discovery facts a frontend needs.
pub struct RunningReceiver {
    shutdown: CancellationToken,
    tasks: TaskTracker,
    hub_handle: DeviceHubHandle,
    cast_server: Arc<CastServer>,
    advertisement: Arc<tokio::sync::Mutex<CastAdvertisement>>,
    /// CastV2 TLS port actually bound.
    pub cast_port: u16,
    /// Eureka HTTP port actually bound.
    pub eureka_http_port: u16,
    /// Eureka HTTPS port actually bound.
    pub eureka_https_port: u16,
    /// mDNS service instance label, for frontend-owned registration.
    pub instance_name: String,
    /// Cast TXT record key/value pairs, for frontend-owned registration.
    pub txt: Vec<(String, String)>,
}

impl RunningReceiver {
    /// A handle for applying certificate-rotation updates to this receiver.
    #[must_use]
    pub fn rotation_handle(&self) -> RotationHandle {
        RotationHandle {
            cast_server: self.cast_server.clone(),
            advertisement: self.advertisement.clone(),
        }
    }

    /// Cooperatively tear this receiver down: stop advertising, stop app
    /// sessions cleanly, then cancel and await the supervised tasks.
    pub async fn shutdown(self) {
        self.advertisement.lock().await.stop();
        let _ = tokio::time::timeout(Duration::from_secs(5), self.hub_handle.shutdown()).await;
        self.shutdown.cancel();
        self.tasks.close();
        if tokio::time::timeout(Duration::from_secs(5), self.tasks.wait())
            .await
            .is_err()
        {
            tracing::warn!("receiver background tasks did not stop within 5s; exiting anyway");
        }
    }
}

/// Assemble and start one receiver instance. Returns once every listener is
/// bound and (when `advertise_mdns`) advertising has started.
pub async fn spawn(params: ReceiverParams) -> Result<RunningReceiver, ReceiverError> {
    let ReceiverParams {
        identity,
        eureka_identity,
        registry,
        player,
        proxy,
        reports,
        http,
        data_dir,
        volume,
        user_agent,
        cast_device_capabilities,
        capabilities,
        resolver,
        bundle,
        crl,
        bind_host,
        cast_port,
        eureka_http_port,
        eureka_https_port,
        advertise_mdns,
        app_ids,
    } = params;

    // Advertisement identity strings, captured before `identity` moves into the hub.
    let adv_name = identity.friendly_name.clone();
    let adv_model = identity.device_model.clone();
    let adv_id = identity.device_id.clone();

    // TLS configs backed by the shared resolver, so a single rotation updates all.
    let cast_tls = server_config(resolver.clone())?;
    let eureka_tls = server_config(resolver)?;

    // Bind all listeners up front (before spawning tasks). A `0` port binds an
    // OS-assigned port, read back so the advertisement announces the real port.
    let cast_listener = TcpListener::bind((bind_host.as_str(), cast_port))
        .await
        .map_err(|source| ReceiverError::Bind {
            what: "cast",
            port: cast_port,
            source,
        })?;
    let cast_port = local_port(&cast_listener, cast_port);

    let eureka = Arc::new(EurekaServer::new(&bundle, eureka_identity)?);
    let eureka_http_listener = TcpListener::bind((bind_host.as_str(), eureka_http_port))
        .await
        .map_err(|source| ReceiverError::Bind {
            what: "eureka http",
            port: eureka_http_port,
            source,
        })?;
    let eureka_http_port = local_port(&eureka_http_listener, eureka_http_port);
    let eureka_https_listener =
        std::net::TcpListener::bind((bind_host.as_str(), eureka_https_port)).map_err(|source| {
            ReceiverError::Bind {
                what: "eureka https",
                port: eureka_https_port,
                source,
            }
        })?;
    eureka_https_listener
        .set_nonblocking(true)
        .map_err(|source| ReceiverError::Bind {
            what: "eureka https",
            port: eureka_https_port,
            source,
        })?;
    let eureka_https_port = eureka_https_listener
        .local_addr()
        .map(|addr| addr.port())
        .unwrap_or(eureka_https_port);

    // Device hub.
    let hub = DeviceHub::new(HubConfig {
        identity,
        registry,
        player,
        proxy,
        http,
        data_dir,
        volume,
        user_agent,
        cast_device_capabilities,
        capabilities,
    });
    let hub_handle = hub.handle();

    let shutdown = CancellationToken::new();
    let tasks = TaskTracker::new();

    // Player reports -> hub.
    {
        let hub_handle = hub_handle.clone();
        let shutdown = shutdown.clone();
        let mut reports = reports;
        tasks.spawn(async move {
            loop {
                tokio::select! {
                    () = shutdown.cancelled() => break,
                    report = reports.recv() => match report {
                        Some(report) => {
                            if hub_handle.send_player_report(report).await.is_err() {
                                break;
                            }
                        }
                        None => break,
                    },
                }
            }
        });
    }

    // CastV2 TLS server.
    let (events_tx, events_rx) = mpsc::channel(64);
    let auth = AuthMaterial {
        bundle: bundle.clone(),
        crl,
    };
    let cast_server = Arc::new(CastServer::new(cast_tls, auth, events_tx));
    spawn_server_forward(&tasks, shutdown.clone(), events_rx, hub_handle.clone());
    tasks.spawn(hub.run());
    {
        let cast_server = cast_server.clone();
        let shutdown = shutdown.clone();
        tasks.spawn(async move {
            tokio::select! {
                () = shutdown.cancelled() => {}
                result = cast_server.serve(cast_listener) => {
                    if let Err(error) = result {
                        tracing::error!(%error, "cast server stopped");
                    }
                }
            }
        });
    }

    // Eureka HTTP + HTTPS.
    {
        let eureka = eureka.clone();
        let shutdown = shutdown.clone();
        tasks.spawn(async move {
            tokio::select! {
                () = shutdown.cancelled() => {}
                result = eureka.serve_http(eureka_http_listener) => {
                    if let Err(error) = result {
                        tracing::error!(%error, "eureka http stopped");
                    }
                }
            }
        });
    }
    {
        let eureka = eureka.clone();
        let shutdown = shutdown.clone();
        tasks.spawn(async move {
            tokio::select! {
                () = shutdown.cancelled() => {}
                result = eureka.serve_https(eureka_https_listener, eureka_tls) => {
                    if let Err(error) = result {
                        tracing::error!(%error, "eureka https stopped");
                    }
                }
            }
        });
    }

    // mDNS advertisement (always computed; only started when advertising from Rust).
    let mut advertisement = CastAdvertisement::new(
        &adv_name,
        &adv_model,
        &adv_id,
        cast_port,
        &bundle.cert_digest_md5(),
        app_ids,
    );
    if advertise_mdns {
        advertisement.start()?;
    }
    let instance_name = advertisement.instance().to_string();
    let txt = txt_pairs(&advertisement);
    let advertisement = Arc::new(tokio::sync::Mutex::new(advertisement));

    Ok(RunningReceiver {
        shutdown,
        tasks,
        hub_handle,
        cast_server,
        advertisement,
        cast_port,
        eureka_http_port,
        eureka_https_port,
        instance_name,
        txt,
    })
}

fn local_port(listener: &TcpListener, fallback: u16) -> u16 {
    listener
        .local_addr()
        .map(|addr| addr.port())
        .unwrap_or(fallback)
}

fn spawn_server_forward(
    tasks: &TaskTracker,
    shutdown: CancellationToken,
    mut events_rx: mpsc::Receiver<ServerEvent>,
    hub_handle: DeviceHubHandle,
) {
    tasks.spawn(async move {
        loop {
            tokio::select! {
                () = shutdown.cancelled() => break,
                event = events_rx.recv() => match event {
                    Some(event) => {
                        if hub_handle.send_server_event(event).await.is_err() {
                            break;
                        }
                    }
                    None => break,
                },
            }
        }
    });
}

/// The advertised Cast TXT record as owned key/value pairs.
fn txt_pairs(advertisement: &CastAdvertisement) -> Vec<(String, String)> {
    advertisement
        .txt()
        .pairs()
        .into_iter()
        .map(|(key, value)| (key.to_string(), value))
        .collect()
}
