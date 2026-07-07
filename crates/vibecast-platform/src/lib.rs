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
mod manager;

use std::collections::HashSet;
use std::net::{IpAddr, UdpSocket};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use vibecast_apps_primevideo::PrimeVideo;
use vibecast_apps_svtplay::SvtPlay;
use vibecast_apps_tv4play::Tv4Play;
use vibecast_apps_viaplay::Viaplay;
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

/// Cast app ids advertised by every receiver (default media / backdrop apps).
const BASE_APP_IDS: &[&str] = &["CC1AD845", "0F5096E8"];

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
}

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
/// player gets its own receiver (see [`PlayerManager`](manager::PlayerManager)).
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
    pub async fn shutdown(self) {
        self.shutdown.cancel();
        // The manager drains and shuts down every per-player receiver on cancel.
        let _ = tokio::time::timeout(Duration::from_secs(15), self.manager).await;
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
    } = inputs;

    let model = config.device.model.clone();
    let bind_host = config.network.bind_host.clone();
    let local_ip = detect_local_ip(&bind_host);

    // --- certificates (shared across every per-player receiver) ---
    let store = CertificateStore::from_manifest_path(&certs_path)?;
    let bundle = store.active_bundle().clone();
    let resolver = CertResolver::new(&bundle)?;

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
        discovery_app_ids,
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
