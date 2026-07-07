//! Shared receiver compose/run orchestration.
//!
//! Every platform binding — the desktop [`vibecast-cli`] binary and the
//! `vibecast-ffi` cdylib (Android/iOS) — assembles the same portable core the
//! same way: load certificates, configure apps, build the HTTP client, start
//! the player bridge, the device hub, the CastV2 TLS server, and the eureka
//! HTTP/HTTPS endpoints, then supervise them under one cancellation token.
//!
//! That composition lives here so the two bindings never drift. What stays in
//! each binding is only what is genuinely platform-specific: argument/settings
//! sourcing, the async runtime, the tracing sink, and *discovery advertisement*
//! (mDNS on desktop via [`CastAdvertisement`], `NsdManager` on Android — hence
//! [`PlatformInputs::advertise_mdns`] and the [`TxtObserver`] hook).
//!
//! [`vibecast-cli`]: ../vibecast_cli/index.html

#![forbid(unsafe_code)]

mod config;

use std::collections::HashSet;
use std::net::{IpAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use vibecast_apps_primevideo::PrimeVideo;
use vibecast_apps_svtplay::SvtPlay;
use vibecast_apps_tv4play::Tv4Play;
use vibecast_apps_viaplay::Viaplay;
use vibecast_bridge::PlayerBridge;
use vibecast_cast::{AuthMaterial, CastServer, ServerEvent};
use vibecast_core::{
    AppRegistry, DeviceHub, DeviceHubHandle, DeviceIdentity, HubConfig, RegistryError,
};
use vibecast_discovery::{CastAdvertisement, DiscoveryError, EurekaIdentity, EurekaServer};
use vibecast_messages::Volume;
use vibecast_sdk::{AppConfig, AppConfigError, AppProvider};
use vibecast_security::{server_config, CertResolver, CertificateStore, SecurityError};

pub use config::{
    CastConfig, CastDeviceCapabilities, Config, ConfigError, DeviceConfig, NetworkConfig,
    VolumeConfig,
};

/// Cast app ids advertised by every receiver (default media / backdrop apps).
const BASE_APP_IDS: &[&str] = &["CC1AD845", "0F5096E8"];

/// Google endpoint for the Cast device CRL (opaque protobuf blob).
const CRL_URL: &str = "https://clients3.google.com/cast/chromecast/device/crl";

/// Platform-supplied inputs that the shared compose logic cannot derive on its
/// own: where state lives, which certificate manifest to load, the stable
/// device id, and whether to advertise over mDNS from Rust.
#[derive(Debug, Clone)]
pub struct PlatformInputs {
    /// Directory holding receiver state (hub scratch, device id, app data).
    pub data_dir: PathBuf,
    /// Absolute path to the certificate manifest.
    pub certs_path: PathBuf,
    /// Stable device id (senders see the same id across restarts).
    pub device_id: String,
    /// Advertise over mDNS from Rust ([`CastAdvertisement`]). Desktop sets
    /// `true`; Android sets `false` and registers via `NsdManager` instead,
    /// consuming [`RunningReceiver::txt`] / [`RunningReceiver::instance_name`].
    pub advertise_mdns: bool,
}

/// Callback invoked (off the receiver's tasks) when the advertised Cast TXT
/// record changes — currently only on certificate rotation. Frontends that own
/// discovery registration (e.g. Android/`NsdManager`) use it to re-register.
pub type TxtObserver = Arc<dyn Fn(Vec<(String, String)>) + Send + Sync>;

/// Errors assembling or starting the receiver.
#[derive(Debug, thiserror::Error)]
pub enum PlatformError {
    /// Loading certificate material or building the TLS config failed.
    #[error("certificate/TLS setup failed")]
    Certs(#[from] SecurityError),
    /// Configuring a bundled app failed.
    #[error("configuring app {app}")]
    AppConfig {
        /// App key that failed to configure.
        app: String,
        /// Underlying config error.
        #[source]
        source: AppConfigError,
    },
    /// Building the app registry failed (e.g. a duplicate app id).
    #[error("building app registry")]
    Registry(#[from] RegistryError),
    /// Building the eureka discovery server failed.
    #[error("discovery setup failed")]
    Discovery(#[from] DiscoveryError),
    /// The derived `CAST-DEVICE-CAPABILITIES` header value was invalid.
    #[error("invalid CAST-DEVICE-CAPABILITIES header")]
    InvalidHeader(#[from] reqwest::header::InvalidHeaderValue),
    /// Building the shared HTTP client failed.
    #[error("building HTTP client")]
    HttpClient(#[source] reqwest::Error),
    /// Starting the player bridge failed.
    #[error("starting player bridge")]
    BridgeStart(#[source] std::io::Error),
    /// Binding a listening socket failed.
    #[error("binding {what} on port {port}")]
    Bind {
        /// Which listener failed to bind.
        what: &'static str,
        /// The port that could not be bound.
        port: u16,
        /// Underlying IO error.
        #[source]
        source: std::io::Error,
    },
}

/// A running receiver: supervised tasks plus the discovery facts a frontend
/// needs to advertise the service itself.
pub struct RunningReceiver {
    shutdown: CancellationToken,
    tasks: TaskTracker,
    hub_handle: DeviceHubHandle,
    bridge: Arc<PlayerBridge>,
    advertisement: Arc<tokio::sync::Mutex<CastAdvertisement>>,
    /// CastV2 TLS port actually bound.
    pub cast_port: u16,
    /// Eureka HTTP port actually bound.
    pub eureka_http_port: u16,
    /// Eureka HTTPS port actually bound.
    pub eureka_https_port: u16,
    /// Player-bridge port actually serving.
    pub player_port: u16,
    /// mDNS service instance label (e.g. `vibecast-<id>`), for `NsdManager`.
    pub instance_name: String,
    /// Cast TXT record key/value pairs, for `NsdManager` attributes.
    pub txt: Vec<(String, String)>,
    /// Best-effort LAN IP the receiver reports to senders.
    pub local_ip: String,
}

impl RunningReceiver {
    /// Cooperatively tear the receiver down: stop advertising, stop app
    /// sessions cleanly, stop the bridge, then cancel and await the supervised
    /// tasks (each step bounded by a 5 s timeout, mirroring the CLI).
    pub async fn shutdown(self) {
        self.advertisement.lock().await.stop();
        let _ = tokio::time::timeout(Duration::from_secs(5), self.hub_handle.shutdown()).await;
        // Bound like the other steps: the bridge's graceful shutdown awaits
        // in-flight connections, and a long-lived `/player` WebSocket would
        // otherwise never drain — hanging the blocking FFI `stop()` forever.
        let _ = tokio::time::timeout(Duration::from_secs(5), self.bridge.stop()).await;
        self.shutdown.cancel();
        self.tasks.close();
        if tokio::time::timeout(Duration::from_secs(5), self.tasks.wait())
            .await
            .is_err()
        {
            tracing::warn!("background tasks did not stop within 5s; exiting anyway");
        }
    }
}

/// Assemble and start the receiver against injected platform inputs.
///
/// Returns once every listener is bound and advertising has started; the
/// receiver then runs on the caller's Tokio runtime until [`RunningReceiver::shutdown`].
pub async fn run(
    config: Config,
    inputs: PlatformInputs,
    on_txt_changed: Option<TxtObserver>,
) -> Result<RunningReceiver, PlatformError> {
    let PlatformInputs {
        data_dir,
        certs_path,
        device_id,
        advertise_mdns,
    } = inputs;

    let friendly_name = config.device.friendly_name.clone();
    let model = config.device.model.clone();
    let bind_host = config.network.bind_host.clone();
    let cast_port = config.network.cast_port;
    let eureka_http_port = config.network.eureka_http_port;
    let eureka_https_port = config.network.eureka_https_port;
    let local_ip = detect_local_ip(&bind_host);

    // --- certificates ---
    let store = CertificateStore::from_manifest_path(&certs_path)?;
    let bundle = store.active_bundle().clone();
    let resolver = CertResolver::new(&bundle)?;
    let cast_tls = server_config(resolver.clone())?;
    let eureka_tls = server_config(resolver.clone())?;

    // --- apps ---
    let providers = build_app_providers(&config)?;
    let mut discovery_app_ids: Vec<String> =
        BASE_APP_IDS.iter().map(|id| (*id).to_string()).collect();
    for provider in &providers {
        for app_id in provider.app_ids() {
            discovery_app_ids.push((*app_id).to_string());
        }
    }
    tracing::info!(
        apps = ?providers
            .iter()
            .map(|p| (p.app_key(), p.display_name(), p.app_ids()))
            .collect::<Vec<_>>(),
        "registered app providers"
    );
    let known_app_keys: HashSet<&str> = providers.iter().map(|p| p.app_key()).collect();
    for app_key in config.apps.keys() {
        if !known_app_keys.contains(app_key.as_str()) {
            tracing::warn!(app = %app_key, "config for an unregistered app is ignored");
        }
    }
    let registry = AppRegistry::new(providers)?;

    // --- shared HTTP client (User-Agent + CAST-DEVICE-CAPABILITIES + timeout) ---
    let http = build_http_client(&config)?;

    // Device-auth CRL: prefer the manifest's embedded CRL, else fetch from Google.
    let crl = resolve_crl(&http, bundle.crl.clone()).await;

    // --- player bridge (constructed now, started below) ---
    let (reports_tx, mut reports_rx) = mpsc::channel(64);
    let bridge = Arc::new(PlayerBridge::new(
        bind_host.clone(),
        config.network.player_port,
        reports_tx,
    ));

    // Bind every listener and build the eureka server up front, before spawning
    // any supervised task or starting the bridge. Every fallible setup step is
    // gathered here, where an early return only has to drop an unspawned socket;
    // once we begin spawning tasks and start the bridge below, nothing else can
    // fail, so a partial start never leaks running tasks or bound ports on the
    // caller's (long-lived, reused) runtime.
    let cast_listener = TcpListener::bind((bind_host.as_str(), cast_port))
        .await
        .map_err(|source| PlatformError::Bind {
            what: "cast",
            port: cast_port,
            source,
        })?;
    let eureka = Arc::new(EurekaServer::new(
        &bundle,
        eureka_identity(&config, &friendly_name, &model, &device_id, &local_ip),
    )?);
    let eureka_http_listener = TcpListener::bind((bind_host.as_str(), eureka_http_port))
        .await
        .map_err(|source| PlatformError::Bind {
            what: "eureka http",
            port: eureka_http_port,
            source,
        })?;
    let eureka_https_listener =
        std::net::TcpListener::bind((bind_host.as_str(), eureka_https_port)).map_err(|source| {
            PlatformError::Bind {
                what: "eureka https",
                port: eureka_https_port,
                source,
            }
        })?;
    eureka_https_listener
        .set_nonblocking(true)
        .map_err(|source| PlatformError::Bind {
            what: "eureka https",
            port: eureka_https_port,
            source,
        })?;

    // Start the bridge last among the fallible steps.
    bridge.start().await.map_err(PlatformError::BridgeStart)?;
    let player_port = bridge.serving_port().unwrap_or(config.network.player_port);

    // --- device hub ---
    let hub = DeviceHub::new(HubConfig {
        identity: DeviceIdentity::new(friendly_name.clone(), model.clone(), device_id.clone()),
        registry,
        renderer: bridge.clone(),
        proxy: bridge.clone(),
        http,
        data_dir,
        volume: initial_volume(&config),
        user_agent: config.cast.user_agent.clone(),
        cast_device_capabilities: config.cast.device_capabilities.header_value(),
        display_width: config.device.display_width,
        display_height: config.device.display_height,
    });
    let hub_handle = hub.handle();

    // All long-running tasks are tracked and share one cancellation token, so
    // shutdown is deterministic rather than relying on process exit.
    let shutdown = CancellationToken::new();
    let tasks = TaskTracker::new();

    // Player reports (primary renderer) -> hub.
    {
        let hub_handle = hub_handle.clone();
        let shutdown = shutdown.clone();
        tasks.spawn(async move {
            loop {
                tokio::select! {
                    () = shutdown.cancelled() => break,
                    report = reports_rx.recv() => match report {
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

    // --- cast TLS server ---
    let (events_tx, events_rx) = mpsc::channel(64);
    let auth = AuthMaterial {
        bundle: bundle.clone(),
        crl: crl.clone(),
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

    // --- eureka discovery (HTTP + HTTPS) ---
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

    // --- discovery advertisement ---
    // The advertisement object always computes the instance name + TXT record;
    // it is only *started* (mDNS responders) when the platform advertises from
    // Rust. Android leaves it stopped and registers via NsdManager using the
    // returned `instance_name`/`txt`.
    let mut advertisement = CastAdvertisement::new(
        &friendly_name,
        &model,
        &device_id,
        cast_port,
        &bundle.cert_digest_md5(),
        discovery_app_ids,
    );
    if advertise_mdns {
        advertisement.start()?;
    }
    let instance_name = advertisement.instance().to_string();
    let txt = txt_pairs(&advertisement);
    let advertisement = Arc::new(tokio::sync::Mutex::new(advertisement));

    // Certificate-rotation loop: hot-swap TLS (both servers share the resolver),
    // device-auth material, and the advertised digest when the active cert
    // expires. On rotation the TXT observer (if any) is notified so a frontend
    // that owns discovery can re-register.
    spawn_cert_rotation(
        &tasks,
        shutdown.clone(),
        store,
        resolver,
        cast_server.clone(),
        advertisement.clone(),
        crl,
        config.network.cert_rotation_poll,
        on_txt_changed,
    );

    Ok(RunningReceiver {
        shutdown,
        tasks,
        hub_handle,
        bridge,
        advertisement,
        cast_port,
        eureka_http_port,
        eureka_https_port,
        player_port,
        instance_name,
        txt,
        local_ip,
    })
}

/// Install the process-default rustls crypto provider (aws-lc-rs). Idempotent —
/// harmless if one is already installed. Each binding calls this once at startup.
pub fn install_default_crypto_provider() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}

/// Load the stable device id from the data dir, generating and persisting one
/// on first run so senders see the same id across restarts.
pub fn load_or_create_device_id(data_dir: &Path) -> String {
    let path = data_dir.join("cast_receiver_device_id");
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let trimmed = existing.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    let id = uuid::Uuid::new_v4().to_string();
    if let Err(error) = std::fs::write(&path, &id) {
        tracing::warn!(%error, path = %path.display(), "could not persist device id");
    }
    id
}

/// The compiled-in app providers, configured from `config.apps`.
fn build_app_providers(config: &Config) -> Result<Vec<Arc<dyn AppProvider>>, PlatformError> {
    let mut providers: Vec<Box<dyn AppProvider>> = vec![
        Box::new(SvtPlay::new()),
        Box::new(Tv4Play::new()),
        Box::new(Viaplay::new()),
        Box::new(PrimeVideo::new()),
    ];

    for provider in &mut providers {
        let app_key = provider.app_key();
        let app_config = match config.apps.get(app_key) {
            Some(value) => AppConfig::from_value(value.clone()),
            None => AppConfig::empty(),
        };
        provider
            .configure(&app_config)
            .map_err(|source| PlatformError::AppConfig {
                app: app_key.to_string(),
                source,
            })?;
    }

    Ok(providers
        .into_iter()
        .map(Arc::<dyn AppProvider>::from)
        .collect())
}

fn initial_volume(config: &Config) -> Volume {
    Volume {
        level: config.volume.level,
        muted: config.volume.muted,
        control_type: Some("attenuation".to_string()),
        step_interval: Some(config.volume.step_interval),
    }
}

fn eureka_identity(
    config: &Config,
    friendly_name: &str,
    model: &str,
    device_id: &str,
    local_ip: &str,
) -> EurekaIdentity {
    let mut identity = EurekaIdentity::new(
        friendly_name.to_string(),
        model.to_string(),
        device_id.to_string(),
        local_ip.to_string(),
    );
    identity.manufacturer = config.device.manufacturer.clone();
    identity.locale = config.device.locale.clone();
    identity.country_code = config.device.country_code.clone();
    identity.build_version = config.cast.build_version.clone();
    identity.build_revision = config.cast.build_revision.clone();
    identity.capabilities = Some(config.device.capabilities.clone());
    identity
}

fn build_http_client(config: &Config) -> Result<reqwest::Client, PlatformError> {
    use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

    let mut headers = HeaderMap::new();
    headers.insert(
        HeaderName::from_static("cast-device-capabilities"),
        HeaderValue::from_str(&config.cast.device_capabilities.header_value())?,
    );
    reqwest::Client::builder()
        .user_agent(&config.cast.user_agent)
        .default_headers(headers)
        .cookie_store(true)
        .timeout(Duration::from_secs_f64(config.network.http_timeout))
        .build()
        .map_err(PlatformError::HttpClient)
}

/// Resolve the device-auth CRL: prefer the manifest's, else fetch from Google.
/// A fetch failure is non-fatal — most senders authenticate without a CRL.
async fn resolve_crl(http: &reqwest::Client, manifest_crl: Option<Vec<u8>>) -> Option<Vec<u8>> {
    if let Some(crl) = manifest_crl {
        tracing::info!(bytes = crl.len(), "using CRL from manifest");
        return Some(crl);
    }
    match fetch_crl(http).await {
        Ok(crl) => {
            tracing::info!(bytes = crl.len(), "fetched Cast CRL");
            Some(crl)
        }
        Err(error) => {
            tracing::warn!(%error, "CRL fetch failed; continuing without a CRL");
            None
        }
    }
}

async fn fetch_crl(http: &reqwest::Client) -> Result<Vec<u8>, reqwest::Error> {
    let response = http.get(CRL_URL).send().await?.error_for_status()?;
    Ok(response.bytes().await?.to_vec())
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

#[allow(clippy::too_many_arguments)]
fn spawn_cert_rotation(
    tasks: &TaskTracker,
    shutdown: CancellationToken,
    mut store: CertificateStore,
    resolver: Arc<CertResolver>,
    cast_server: Arc<CastServer>,
    advertisement: Arc<tokio::sync::Mutex<CastAdvertisement>>,
    startup_crl: Option<Vec<u8>>,
    poll_seconds: f64,
    on_txt_changed: Option<TxtObserver>,
) {
    tasks.spawn(async move {
        let period = Duration::from_secs_f64(poll_seconds.max(1.0));
        let mut ticker = tokio::time::interval(period);
        ticker.tick().await; // consume the immediate first tick
        loop {
            tokio::select! {
                () = shutdown.cancelled() => break,
                _ = ticker.tick() => {}
            }
            let rotated = match store.rotate_if_needed(unix_now()) {
                Ok(Some(bundle)) => bundle.clone(),
                Ok(None) => continue,
                Err(error) => {
                    tracing::error!(%error, "certificate rotation check failed");
                    continue;
                }
            };
            if let Err(error) = resolver.update(&rotated) {
                tracing::error!(%error, "failed to hot-swap TLS certificate");
                continue;
            }
            let crl = rotated.crl.clone().or_else(|| startup_crl.clone());
            cast_server.update_auth(AuthMaterial {
                bundle: rotated.clone(),
                crl,
            });
            let pairs = {
                let mut advertisement = advertisement.lock().await;
                if let Err(error) = advertisement.update_cert_digest(&rotated.cert_digest_md5()) {
                    tracing::error!(%error, "failed to update advertised certificate digest");
                }
                txt_pairs(&advertisement)
            };
            if let Some(observer) = &on_txt_changed {
                observer(pairs);
            }
            tracing::info!("rotated active certificate (TLS + device-auth + discovery)");
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

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Best-effort LAN IP: use an explicit bind IP if given, else the address the
/// kernel would route to the internet (no packets are actually sent).
pub fn detect_local_ip(bind_host: &str) -> String {
    if bind_host != "0.0.0.0" && bind_host != "::" && !bind_host.is_empty() {
        if let Ok(ip) = bind_host.parse::<IpAddr>() {
            return ip.to_string();
        }
    }
    UdpSocket::bind(("0.0.0.0", 0))
        .and_then(|socket| {
            socket.connect(("8.8.8.8", 80))?;
            socket.local_addr()
        })
        .map(|addr| addr.ip().to_string())
        .unwrap_or_else(|_| "127.0.0.1".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_providers_are_registered() {
        let providers = build_app_providers(&Config::default()).unwrap();
        let keys: Vec<&str> = providers.iter().map(|app| app.app_key()).collect();
        assert!(keys.contains(&"svtplay"));
        assert!(keys.contains(&"tv4play"));
        assert!(keys.contains(&"viaplay"));
        assert!(keys.contains(&"primevideo"));
    }

    #[test]
    fn explicit_bind_ip_is_used_as_local_ip() {
        assert_eq!(detect_local_ip("192.168.1.50"), "192.168.1.50");
    }

    #[test]
    fn wildcard_bind_resolves_to_a_valid_ip() {
        let ip = detect_local_ip("0.0.0.0");
        assert!(ip.parse::<IpAddr>().is_ok(), "not an IP: {ip}");
    }
}
