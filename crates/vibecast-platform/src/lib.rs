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
//! (mDNS on desktop via [`CastAdvertisement`](vibecast_discovery::CastAdvertisement),
//! `NsdManager` on Android — hence [`PlatformInputs::advertise_mdns`] and the
//! [`PlayerObserver`] hook).
//!
//! [`vibecast-cli`]: ../vibecast_cli/index.html

#![forbid(unsafe_code)]

mod config;
mod manager;

use std::collections::HashSet;
use std::io::Write;
use std::net::{IpAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use vibecast_apps_primevideo::PrimeVideo;
use vibecast_apps_svtplay::SvtPlay;
use vibecast_apps_tv4play::Tv4Play;
use vibecast_apps_viaplay::Viaplay;
use vibecast_apps_youtube::YouTube;
use vibecast_bridge::PlayerBridge;
use vibecast_core::{AppRegistry, RegistryError};
use vibecast_discovery::DiscoveryError;
use vibecast_messages::Volume;
use vibecast_receiver::ReceiverError;
use vibecast_sdk::{AppConfig, AppConfigError, AppProvider};
use vibecast_security::{CertResolver, CertificateStore, SecurityError};

use manager::{EurekaConfig, ManagerConfig, PlayerManager};

pub use config::{
    CastConfig, CastDeviceCapabilities, Config, ConfigError, DeviceConfig, NetworkConfig,
    VolumeConfig,
};
pub use manager::{PlayerObserver, PlayerStarted};

const INSTALLATION_ID_FILE: &str = "installation_id";

/// Google endpoint for the Cast device CRL (opaque protobuf blob).
const CRL_URL: &str = "https://clients3.google.com/cast/chromecast/device/crl";

/// Platform-supplied inputs that the shared compose logic cannot derive on its
/// own: where state lives, which certificate manifest to load, and whether each
/// per-player receiver advertises over mDNS from Rust.
#[derive(Debug, Clone)]
pub struct PlatformInputs {
    /// Directory holding receiver state (hub scratch, app data).
    pub data_dir: PathBuf,
    /// Absolute path to the certificate manifest.
    pub certs_path: PathBuf,
    /// Advertise each per-player receiver over mDNS from Rust. Desktop sets
    /// `true`; Android sets `false` and registers via `NsdManager` per player,
    /// consuming the facts delivered to [`PlayerObserver::on_player_started`].
    pub advertise_mdns: bool,
    /// LAN IP the receiver reports to senders (eureka `ip_address`). When
    /// `None`, it is derived from the routed interface via [`detect_local_ip`]
    /// — a desktop-oriented heuristic. Frontends that resolve their own LAN
    /// address (e.g. Android, by enumerating `NetworkInterface`s for the first
    /// site-local IPv4) should supply it explicitly rather than relying on the
    /// heuristic.
    pub local_ip: Option<String>,
}

/// Errors assembling or starting the receiver.
#[derive(Debug, thiserror::Error)]
pub enum PlatformError {
    /// Reading persistent receiver state failed.
    #[error("reading receiver state {path}")]
    StateRead {
        /// State file path.
        path: PathBuf,
        /// Underlying IO error.
        #[source]
        source: std::io::Error,
    },
    /// Persistent receiver state was invalid.
    #[error("invalid installation id in {path}")]
    InvalidInstallationId {
        /// State file path.
        path: PathBuf,
        /// UUID parse error.
        #[source]
        source: uuid::Error,
    },
    /// Persisting receiver state failed.
    #[error("writing receiver state {path}")]
    StateWrite {
        /// State file path.
        path: PathBuf,
        /// Underlying IO error.
        #[source]
        source: std::io::Error,
    },
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

impl From<ReceiverError> for PlatformError {
    fn from(error: ReceiverError) -> Self {
        match error {
            ReceiverError::Certs(source) => PlatformError::Certs(source),
            ReceiverError::Discovery(source) => PlatformError::Discovery(source),
            ReceiverError::Bind { what, port, source } => {
                PlatformError::Bind { what, port, source }
            }
        }
    }
}

/// A running vibecast server: the shared player bridge plus the per-player
/// orchestrator. No Cast device exists until a player registers; each registered
/// player gets its own receiver (see the per-player orchestrator, `PlayerManager`).
pub struct RunningReceiver {
    shutdown: CancellationToken,
    manager: tokio::task::JoinHandle<()>,
    bridge: Arc<PlayerBridge>,
    /// Player-bridge port actually serving (players connect here to register).
    pub player_port: u16,
    /// Best-effort LAN IP the receiver reports to senders.
    pub local_ip: String,
}

impl RunningReceiver {
    /// Cooperatively tear everything down: cancel the orchestrator (which stops
    /// every per-player receiver), then stop the shared bridge.
    pub async fn shutdown(mut self) {
        self.shutdown.cancel();
        // The manager drains and shuts down every per-player receiver on cancel.
        // Await by reference so a timeout doesn't just detach the task: if it
        // overruns, abort it so orphaned per-player receivers can't keep running
        // on the (long-lived, FFI) runtime after `shutdown()` returns.
        if tokio::time::timeout(Duration::from_secs(15), &mut self.manager)
            .await
            .is_err()
        {
            tracing::warn!("orchestrator did not stop within 15s; aborting it");
            self.manager.abort();
        }
        // Bound: the bridge's graceful shutdown awaits in-flight connections, and
        // a long-lived `/player` WebSocket would otherwise never drain.
        let _ = tokio::time::timeout(Duration::from_secs(5), self.bridge.stop()).await;
    }
}

/// Start the vibecast server against injected platform inputs.
///
/// Returns once the shared player bridge is listening and the orchestrator is
/// running. Players then register over the bridge and each is given its own Cast
/// receiver; per-player lifecycle is surfaced through `observer`.
pub async fn run(
    config: Config,
    inputs: PlatformInputs,
    observer: Option<Arc<dyn PlayerObserver>>,
) -> Result<RunningReceiver, PlatformError> {
    let PlatformInputs {
        data_dir,
        certs_path,
        advertise_mdns,
        local_ip,
    } = inputs;

    let model = config.device.model.clone();
    let bind_host = config.network.bind_host.clone();
    let local_ip = local_ip.unwrap_or_else(|| detect_local_ip(&bind_host));
    std::fs::create_dir_all(&data_dir).map_err(|source| PlatformError::StateWrite {
        path: data_dir.clone(),
        source,
    })?;
    let installation_id = load_or_create_installation_id(&data_dir)?;

    // --- certificates (shared across every per-player receiver) ---
    let store = CertificateStore::from_manifest_path(&certs_path)?;
    let bundle = store.active_bundle().clone();
    let resolver = CertResolver::new(&bundle)?;

    // --- apps ---
    let providers = build_app_providers(&config)?;
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

    // --- shared player bridge (registration + proxy hosting) ---
    let (events_tx, events_rx) = mpsc::channel(64);
    let bridge = Arc::new(PlayerBridge::new(
        bind_host.clone(),
        config.network.player_port,
        events_tx,
    ));
    bridge.start().await.map_err(PlatformError::BridgeStart)?;
    let player_port = bridge.serving_port().unwrap_or(config.network.player_port);

    // --- per-player orchestrator ---
    let manager_config = ManagerConfig {
        bridge: bridge.clone(),
        registry,
        installation_id,
        http,
        data_dir,
        model,
        volume: initial_volume(&config),
        user_agent: config.cast.user_agent.clone(),
        cast_device_capabilities: config.cast.device_capabilities.header_value(),
        resolver,
        store,
        crl,
        bind_host,
        local_ip: local_ip.clone(),
        eureka: EurekaConfig {
            manufacturer: config.device.manufacturer.clone(),
            locale: config.device.locale.clone(),
            country_code: config.device.country_code.clone(),
            build_version: config.cast.build_version.clone(),
            build_revision: config.cast.build_revision.clone(),
            capabilities: config.device.capabilities.clone(),
        },
        advertise_mdns,
        cert_rotation_poll: config.network.cert_rotation_poll,
        observer,
    };
    let shutdown = CancellationToken::new();
    let manager = PlayerManager::new(manager_config, bundle);
    let manager_task = tokio::spawn(manager.run(events_rx, shutdown.clone()));

    Ok(RunningReceiver {
        shutdown,
        manager: manager_task,
        bridge,
        player_port,
        local_ip,
    })
}

fn load_or_create_installation_id(data_dir: &Path) -> Result<uuid::Uuid, PlatformError> {
    let path = data_dir.join(INSTALLATION_ID_FILE);
    match std::fs::read_to_string(&path) {
        Ok(value) => parse_installation_id(&path, &value),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => create_installation_id(&path),
        Err(source) => Err(PlatformError::StateRead { path, source }),
    }
}

fn parse_installation_id(path: &Path, value: &str) -> Result<uuid::Uuid, PlatformError> {
    uuid::Uuid::parse_str(value.trim()).map_err(|source| PlatformError::InvalidInstallationId {
        path: path.to_path_buf(),
        source,
    })
}

fn create_installation_id(path: &Path) -> Result<uuid::Uuid, PlatformError> {
    let id = uuid::Uuid::new_v4();
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut file) => {
            file.write_all(id.to_string().as_bytes())
                .map_err(|source| PlatformError::StateWrite {
                    path: path.to_path_buf(),
                    source,
                })?;
            Ok(id)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            read_concurrently_created_installation_id(path)
        }
        Err(source) => Err(PlatformError::StateWrite {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn read_concurrently_created_installation_id(path: &Path) -> Result<uuid::Uuid, PlatformError> {
    // The winning process creates the file before writing its UUID. Give that
    // short window time to close rather than treating an empty file as corrupt.
    for _ in 0..10 {
        let value = std::fs::read_to_string(path).map_err(|source| PlatformError::StateRead {
            path: path.to_path_buf(),
            source,
        })?;
        if let Ok(id) = uuid::Uuid::parse_str(value.trim()) {
            return Ok(id);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let value = std::fs::read_to_string(path).map_err(|source| PlatformError::StateRead {
        path: path.to_path_buf(),
        source,
    })?;
    parse_installation_id(path, &value)
}

/// Install the process-default rustls crypto provider (aws-lc-rs). Idempotent —
/// harmless if one is already installed. Each binding calls this once at startup.
pub fn install_default_crypto_provider() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}

/// The compiled-in app providers, configured from `config.apps`.
fn build_app_providers(config: &Config) -> Result<Vec<Arc<dyn AppProvider>>, PlatformError> {
    let mut providers: Vec<Box<dyn AppProvider>> = vec![
        Box::new(SvtPlay::new()),
        Box::new(Tv4Play::new()),
        Box::new(Viaplay::new()),
        Box::new(PrimeVideo::new()),
        Box::new(YouTube::new()),
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
        assert!(keys.contains(&"youtube"));
    }

    #[test]
    fn installation_id_is_created_and_reused() {
        let data_dir = std::env::temp_dir().join(format!(
            "vibecast-platform-installation-id-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&data_dir).unwrap();

        let first = load_or_create_installation_id(&data_dir).unwrap();
        let second = load_or_create_installation_id(&data_dir).unwrap();
        assert_eq!(first, second);
        assert_eq!(
            std::fs::read_to_string(data_dir.join(INSTALLATION_ID_FILE)).unwrap(),
            first.to_string()
        );

        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[test]
    fn invalid_installation_id_is_rejected() {
        let data_dir = std::env::temp_dir().join(format!(
            "vibecast-platform-invalid-installation-id-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&data_dir).unwrap();
        std::fs::write(data_dir.join(INSTALLATION_ID_FILE), "not-a-uuid").unwrap();

        assert!(matches!(
            load_or_create_installation_id(&data_dir),
            Err(PlatformError::InvalidInstallationId { .. })
        ));

        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[test]
    fn concurrent_installation_id_creation_selects_one_id() {
        let data_dir = std::env::temp_dir().join(format!(
            "vibecast-platform-concurrent-installation-id-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&data_dir).unwrap();
        let barrier = Arc::new(std::sync::Barrier::new(2));

        let handles: Vec<_> = (0..2)
            .map(|_| {
                let data_dir = data_dir.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    load_or_create_installation_id(&data_dir).unwrap()
                })
            })
            .collect();
        let ids: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();

        assert_eq!(ids[0], ids[1]);
        assert_eq!(ids[0], load_or_create_installation_id(&data_dir).unwrap());
        std::fs::remove_dir_all(data_dir).unwrap();
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

    #[test]
    fn injected_local_ip_is_used_verbatim_else_heuristic() {
        // Mirrors how `run` resolves the reported IP from `PlatformInputs`.
        fn resolve(local_ip: Option<String>, bind_host: &str) -> String {
            local_ip.unwrap_or_else(|| detect_local_ip(bind_host))
        }

        // A frontend-supplied address is used as-is (even one the routing
        // heuristic would never pick), so a multi-interface host reports it.
        assert_eq!(
            resolve(Some("10.42.0.7".to_string()), "0.0.0.0"),
            "10.42.0.7"
        );

        // `None` falls back to the heuristic, which yields a valid IP.
        let fallback = resolve(None, "0.0.0.0");
        assert!(fallback.parse::<IpAddr>().is_ok(), "not an IP: {fallback}");
    }
}
