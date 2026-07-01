//! vibecast receiver binary.
//!
//! Wires the portable core crates into a runnable native receiver: the CastV2
//! TLS server, the device hub + coordinator, the player bridge, and mDNS +
//! eureka discovery. Configuration (TOML + CLI overrides), certificate loading,
//! and the shared HTTP client live here (the platform binary); the portable
//! core receives only typed, injected inputs.

mod config;

use std::collections::HashSet;
use std::net::{IpAddr, UdpSocket};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use clap::Parser;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tracing_subscriber::EnvFilter;

use vibecast_apps_svtplay::SvtPlay;
use vibecast_apps_tv4play::Tv4Play;
use vibecast_bridge::PlayerBridge;
use vibecast_cast::{AuthMaterial, CastServer, ServerEvent};
use vibecast_core::{AppRegistry, DeviceHub, DeviceIdentity, HubConfig, HubEvent};
use vibecast_discovery::{CastAdvertisement, EurekaIdentity, EurekaServer};
use vibecast_messages::Volume;
use vibecast_sdk::AppProvider;
use vibecast_security::{server_config, CertResolver, CertificateStore};

use crate::config::Config;

/// Cast app ids advertised by every receiver (default media / backdrop apps).
const BASE_APP_IDS: &[&str] = &["CC1AD845", "0F5096E8"];

/// Google endpoint for the Cast device CRL (opaque protobuf blob).
const CRL_URL: &str = "https://clients3.google.com/cast/chromecast/device/crl";

/// Command-line arguments. Flags override matching `config.toml` values.
#[derive(Debug, Parser)]
#[command(name = "vibecast", about = "A native Google Cast receiver")]
struct Args {
    /// Certificate manifest path (overrides `[device].certs`; relative paths
    /// resolve from the data dir).
    #[arg(long)]
    certs: Option<PathBuf>,

    /// Data directory holding `config.toml` and receiver state
    /// (default: `$HOME/.vibecast`).
    #[arg(long)]
    data_dir: Option<PathBuf>,

    /// Override the configured friendly name.
    #[arg(long)]
    name: Option<String>,

    /// Override the configured device model.
    #[arg(long)]
    model: Option<String>,

    /// Override the configured bind host.
    #[arg(long)]
    bind_host: Option<String>,

    /// Override the CastV2 TLS port (standard 8009); advertised over mDNS.
    #[arg(long)]
    cast_port: Option<u16>,

    /// Stable device id (default: a random UUID).
    #[arg(long)]
    device_id: Option<String>,

    /// Log level (`trace|debug|info|warn|error`); overrides `RUST_LOG`.
    #[arg(long)]
    log_level: Option<String>,
}

/// The compiled-in app providers. Adding an app appends one line here.
fn apps() -> Vec<Arc<dyn AppProvider>> {
    vec![Arc::new(SvtPlay::new()), Arc::new(Tv4Play::new())]
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    init_tracing(args.log_level.as_deref());

    // Ensure a process-default rustls crypto provider exists (aws-lc-rs);
    // harmless if one is already installed.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    run(args).await
}

async fn run(args: Args) -> anyhow::Result<()> {
    let data_dir = args.data_dir.clone().unwrap_or_else(default_data_dir);
    std::fs::create_dir_all(&data_dir)
        .with_context(|| format!("creating data dir {}", data_dir.display()))?;

    let config = Config::load(&data_dir)?;

    // CLI flags override config.
    let friendly_name = args
        .name
        .clone()
        .unwrap_or_else(|| config.device.friendly_name.clone());
    let model = args
        .model
        .clone()
        .unwrap_or_else(|| config.device.model.clone());
    let bind_host = args
        .bind_host
        .clone()
        .unwrap_or_else(|| config.network.bind_host.clone());
    let cast_port = args.cast_port.unwrap_or(config.network.cast_port);
    let eureka_http_port = config.network.eureka_http_port;
    let eureka_https_port = config.network.eureka_https_port;
    let device_id = match &args.device_id {
        Some(id) => id.clone(),
        None => load_or_create_device_id(&data_dir),
    };
    let local_ip = detect_local_ip(&bind_host);

    // --- certificates ---
    let certs_path = resolve_certs_path(&args, &config, &data_dir);
    let store = CertificateStore::from_manifest_path(&certs_path)
        .with_context(|| format!("loading certificate manifest {}", certs_path.display()))?;
    let bundle = store.active_bundle().clone();
    let resolver = CertResolver::new(&bundle).context("building TLS resolver")?;
    let cast_tls = server_config(resolver.clone()).context("cast TLS config")?;
    let eureka_tls = server_config(resolver.clone()).context("eureka TLS config")?;

    // --- apps ---
    let providers = apps();
    let mut discovery_app_ids: Vec<String> =
        BASE_APP_IDS.iter().map(|id| (*id).to_string()).collect();
    for provider in &providers {
        for app_id in provider.app_ids() {
            discovery_app_ids.push((*app_id).to_string());
        }
    }
    let known_app_keys: HashSet<&str> = providers.iter().map(|p| p.app_key()).collect();
    for app_key in config.apps.keys() {
        if !known_app_keys.contains(app_key.as_str()) {
            tracing::warn!(app = %app_key, "config for an unregistered app is ignored");
        }
    }
    let registry = AppRegistry::new(providers);

    // --- shared HTTP client (User-Agent + CAST-DEVICE-CAPABILITIES + timeout) ---
    let http = build_http_client(&config).context("building HTTP client")?;

    // Device-auth CRL: prefer the manifest's embedded CRL, else fetch from Google.
    let crl = resolve_crl(&http, bundle.crl.clone()).await;

    // --- player bridge ---
    let (reports_tx, mut reports_rx) = mpsc::channel(64);
    let bridge = Arc::new(PlayerBridge::new(
        bind_host.clone(),
        config.network.player_port,
        reports_tx,
    ));
    bridge.start().await.context("starting player bridge")?;
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
    let hub_tx = hub.sender();

    // Player reports (primary renderer) -> hub.
    {
        let hub_tx = hub_tx.clone();
        tokio::spawn(async move {
            while let Some(report) = reports_rx.recv().await {
                if hub_tx.send(HubEvent::Report(report)).await.is_err() {
                    break;
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
    spawn_server_forward(events_rx, hub_tx.clone());
    tokio::spawn(hub.run());

    let cast_listener = TcpListener::bind((bind_host.as_str(), cast_port))
        .await
        .with_context(|| format!("binding cast port {cast_port}"))?;
    {
        let cast_server = cast_server.clone();
        tokio::spawn(async move {
            if let Err(error) = cast_server.serve(cast_listener).await {
                tracing::error!(%error, "cast server stopped");
            }
        });
    }

    // --- eureka discovery (HTTP + HTTPS) ---
    let eureka = Arc::new(
        EurekaServer::new(
            &bundle,
            eureka_identity(&config, &friendly_name, &model, &device_id, &local_ip),
        )
        .context("building eureka server")?,
    );
    {
        let eureka = eureka.clone();
        let listener = TcpListener::bind((bind_host.as_str(), eureka_http_port))
            .await
            .with_context(|| format!("binding eureka http port {eureka_http_port}"))?;
        tokio::spawn(async move {
            if let Err(error) = eureka.serve_http(listener).await {
                tracing::error!(%error, "eureka http stopped");
            }
        });
    }
    {
        let eureka = eureka.clone();
        let listener = std::net::TcpListener::bind((bind_host.as_str(), eureka_https_port))
            .with_context(|| format!("binding eureka https port {eureka_https_port}"))?;
        listener
            .set_nonblocking(true)
            .context("eureka https nonblocking")?;
        tokio::spawn(async move {
            if let Err(error) = eureka.serve_https(listener, eureka_tls).await {
                tracing::error!(%error, "eureka https stopped");
            }
        });
    }

    // --- mDNS advertisement ---
    let mut advertisement = CastAdvertisement::new(
        &friendly_name,
        &model,
        &device_id,
        cast_port,
        &bundle.cert_digest_md5(),
        discovery_app_ids,
    );
    advertisement
        .start()
        .context("starting mDNS advertisement")?;
    let advertisement = Arc::new(tokio::sync::Mutex::new(advertisement));

    // Certificate-rotation loop: hot-swap TLS (both servers share the resolver),
    // device-auth material, and the mDNS digest when the active cert expires.
    spawn_cert_rotation(
        store,
        resolver,
        cast_server.clone(),
        advertisement.clone(),
        crl,
        config.network.cert_rotation_poll,
    );

    tracing::info!(
        name = %friendly_name,
        ip = %local_ip,
        cast_port,
        player = format_args!("http://{local_ip}:{player_port}/"),
        "vibecast receiver started"
    );

    tokio::signal::ctrl_c()
        .await
        .context("waiting for shutdown signal")?;
    tracing::info!("shutting down");
    advertisement.lock().await.stop();
    // Tear down app sessions cleanly (app on_stop + proxy unregister) before exit.
    let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
    if hub_tx.send(HubEvent::Shutdown(ack_tx)).await.is_ok() {
        let _ = tokio::time::timeout(Duration::from_secs(5), ack_rx).await;
    }
    bridge.stop().await;
    Ok(())
}

fn spawn_cert_rotation(
    mut store: CertificateStore,
    resolver: Arc<CertResolver>,
    cast_server: Arc<CastServer>,
    advertisement: Arc<tokio::sync::Mutex<CastAdvertisement>>,
    startup_crl: Option<Vec<u8>>,
    poll_seconds: f64,
) {
    tokio::spawn(async move {
        let period = Duration::from_secs_f64(poll_seconds.max(1.0));
        let mut ticker = tokio::time::interval(period);
        ticker.tick().await; // consume the immediate first tick
        loop {
            ticker.tick().await;
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
            if let Err(error) = advertisement
                .lock()
                .await
                .update_cert_digest(&rotated.cert_digest_md5())
            {
                tracing::error!(%error, "failed to update mDNS certificate digest");
            }
            tracing::info!("rotated active certificate (TLS + device-auth + mDNS)");
        }
    });
}

/// Load the stable device id from the data dir, generating and persisting one
/// on first run so senders see the same id across restarts.
fn load_or_create_device_id(data_dir: &std::path::Path) -> String {
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

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn resolve_certs_path(args: &Args, config: &Config, data_dir: &std::path::Path) -> PathBuf {
    if let Some(certs) = &args.certs {
        return certs.clone();
    }
    let configured = PathBuf::from(&config.device.certs);
    if configured.is_absolute() {
        configured
    } else {
        data_dir.join(configured)
    }
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

fn build_http_client(config: &Config) -> anyhow::Result<reqwest::Client> {
    use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

    let mut headers = HeaderMap::new();
    headers.insert(
        HeaderName::from_static("cast-device-capabilities"),
        HeaderValue::from_str(&config.cast.device_capabilities.header_value())
            .context("CAST-DEVICE-CAPABILITIES header")?,
    );
    reqwest::Client::builder()
        .user_agent(&config.cast.user_agent)
        .default_headers(headers)
        .timeout(Duration::from_secs_f64(config.network.http_timeout))
        .build()
        .context("building reqwest client")
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

async fn fetch_crl(http: &reqwest::Client) -> anyhow::Result<Vec<u8>> {
    let response = http.get(CRL_URL).send().await?.error_for_status()?;
    Ok(response.bytes().await?.to_vec())
}

fn spawn_server_forward(
    mut events_rx: mpsc::Receiver<ServerEvent>,
    hub_tx: mpsc::Sender<HubEvent>,
) {
    tokio::spawn(async move {
        while let Some(event) = events_rx.recv().await {
            if hub_tx.send(HubEvent::Server(event)).await.is_err() {
                break;
            }
        }
    });
}

fn init_tracing(log_level: Option<&str>) {
    let filter = match log_level {
        Some(level) => EnvFilter::new(level),
        None => EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
    };
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

fn default_data_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join(".vibecast"))
        .unwrap_or_else(|| PathBuf::from(".vibecast"))
}

/// Best-effort LAN IP: use an explicit bind IP if given, else the address the
/// kernel would route to the internet (no packets are actually sent).
fn detect_local_ip(bind_host: &str) -> String {
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
    fn parses_optional_overrides() {
        let args = Args::try_parse_from([
            "vibecast",
            "--certs",
            "/certs.json",
            "--name",
            "Living Room",
        ])
        .unwrap();
        assert_eq!(args.certs, Some(PathBuf::from("/certs.json")));
        assert_eq!(args.name.as_deref(), Some("Living Room"));
        assert_eq!(args.model, None);
    }

    #[test]
    fn no_args_is_valid() {
        let args = Args::try_parse_from(["vibecast"]).unwrap();
        assert_eq!(args.certs, None);
        assert_eq!(args.name, None);
    }

    #[test]
    fn certs_path_falls_back_to_config_relative_to_data_dir() {
        let args = Args::try_parse_from(["vibecast"]).unwrap();
        let config = Config::default();
        let path = resolve_certs_path(&args, &config, std::path::Path::new("/data"));
        assert_eq!(path, PathBuf::from("/data/certs.json"));
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
    fn apps_are_registered() {
        let keys: Vec<&str> = apps().iter().map(|app| app.app_key()).collect();
        assert!(keys.contains(&"svtplay"));
        assert!(keys.contains(&"tv4play"));
    }
}
