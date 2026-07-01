//! vibecast receiver binary.
//!
//! Wires the portable core crates into a runnable native receiver: the CastV2
//! TLS server, the device hub + coordinator, the player bridge, and mDNS +
//! eureka discovery. No Python interpreter is involved.

use std::net::{IpAddr, UdpSocket};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use clap::Parser;
use tokio::net::TcpListener;
use tokio::sync::mpsc;

use vibecast_apps_svtplay::SvtPlay;
use vibecast_bridge::PlayerBridge;
use vibecast_cast::{AuthMaterial, CastServer, ServerEvent};
use vibecast_core::{AppRegistry, DeviceHub, DeviceIdentity, HubConfig, HubEvent};
use vibecast_discovery::{CastAdvertisement, EurekaIdentity, EurekaServer};
use vibecast_messages::Volume;
use vibecast_sdk::AppProvider;
use vibecast_security::{server_config, CertResolver, CertificateStore};

const CAST_PORT: u16 = 8009;
const EUREKA_HTTPS_PORT: u16 = 8443;
const EUREKA_HTTP_PORT: u16 = 8008;
const PLAYER_PORT: u16 = 8010;

/// Cast app ids advertised by every receiver (default media / backdrop apps).
const BASE_APP_IDS: &[&str] = &["CC1AD845", "0F5096E8"];

/// Command-line arguments.
#[derive(Debug, Parser)]
#[command(name = "vibecast", about = "A native Google Cast receiver")]
struct Args {
    /// Path to the device-auth certificate manifest (JSON).
    #[arg(long)]
    manifest: PathBuf,

    /// Friendly name shown to senders.
    #[arg(long, default_value = "vibecast")]
    name: String,

    /// Device model string.
    #[arg(long, default_value = "Chromecast")]
    model: String,

    /// Address to bind all listeners to.
    #[arg(long, default_value = "0.0.0.0")]
    bind_host: String,

    /// Data directory (default: `$HOME/.vibecast`).
    #[arg(long)]
    data_dir: Option<PathBuf>,

    /// Stable device id (default: a random UUID).
    #[arg(long)]
    device_id: Option<String>,
}

/// The compiled-in app providers. Adding an app appends one line here.
fn apps() -> Vec<Arc<dyn AppProvider>> {
    vec![Arc::new(SvtPlay::new())]
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // Ensure a process-default rustls crypto provider exists (aws-lc-rs);
    // harmless if one is already installed.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let args = Args::parse();
    run(args).await
}

async fn run(args: Args) -> anyhow::Result<()> {
    let data_dir = args.data_dir.clone().unwrap_or_else(default_data_dir);
    std::fs::create_dir_all(&data_dir)
        .with_context(|| format!("creating data dir {}", data_dir.display()))?;
    let device_id = args
        .device_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let local_ip = detect_local_ip(&args.bind_host);

    // --- certificates ---
    let store = CertificateStore::from_manifest_path(&args.manifest)
        .with_context(|| format!("loading manifest {}", args.manifest.display()))?;
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
    let registry = AppRegistry::new(providers);

    let http = reqwest::Client::new();

    // --- player bridge ---
    let (reports_tx, mut reports_rx) = mpsc::channel(64);
    let bridge = Arc::new(PlayerBridge::new(
        args.bind_host.clone(),
        PLAYER_PORT,
        reports_tx,
    ));
    bridge.start().await.context("starting player bridge")?;

    // --- device hub ---
    let hub = DeviceHub::new(HubConfig {
        identity: DeviceIdentity::new(args.name.clone(), args.model.clone(), device_id.clone()),
        registry,
        renderer: bridge.clone(),
        proxy: bridge.clone(),
        http,
        data_dir,
        volume: default_volume(),
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
        crl: bundle.crl.clone(),
    };
    let cast_server = Arc::new(CastServer::new(cast_tls, auth, events_tx));

    // Cast transport events -> hub.
    spawn_server_forward(events_rx, hub_tx.clone());
    tokio::spawn(hub.run());

    let cast_listener = TcpListener::bind((args.bind_host.as_str(), CAST_PORT))
        .await
        .with_context(|| format!("binding cast port {CAST_PORT}"))?;
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
            EurekaIdentity::new(
                args.name.clone(),
                args.model.clone(),
                device_id.clone(),
                local_ip.clone(),
            ),
        )
        .context("building eureka server")?,
    );
    {
        let eureka = eureka.clone();
        let listener = TcpListener::bind((args.bind_host.as_str(), EUREKA_HTTP_PORT))
            .await
            .with_context(|| format!("binding eureka http port {EUREKA_HTTP_PORT}"))?;
        tokio::spawn(async move {
            if let Err(error) = eureka.serve_http(listener).await {
                tracing::error!(%error, "eureka http stopped");
            }
        });
    }
    {
        let eureka = eureka.clone();
        let listener = std::net::TcpListener::bind((args.bind_host.as_str(), EUREKA_HTTPS_PORT))
            .with_context(|| format!("binding eureka https port {EUREKA_HTTPS_PORT}"))?;
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
        &args.name,
        &args.model,
        &device_id,
        CAST_PORT,
        &bundle.cert_digest_md5(),
        discovery_app_ids,
    );
    advertisement
        .start()
        .context("starting mDNS advertisement")?;

    tracing::info!(
        name = %args.name,
        ip = %local_ip,
        cast_port = CAST_PORT,
        player = format_args!("http://{local_ip}:{PLAYER_PORT}/"),
        "vibecast receiver started"
    );

    tokio::signal::ctrl_c()
        .await
        .context("waiting for shutdown signal")?;
    tracing::info!("shutting down");
    advertisement.stop();
    bridge.stop().await;
    Ok(())
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

fn default_volume() -> Volume {
    Volume {
        level: 1.0,
        muted: false,
        control_type: Some("attenuation".to_string()),
        step_interval: Some(0.05),
    }
}

fn default_data_dir() -> PathBuf {
    dirs_home()
        .map(|home| home.join(".vibecast"))
        .unwrap_or_else(|| PathBuf::from(".vibecast"))
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
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
    fn parses_required_and_named_args() {
        let args = Args::try_parse_from([
            "vibecast",
            "--manifest",
            "/certs.json",
            "--name",
            "Living Room",
        ])
        .unwrap();
        assert_eq!(args.manifest, PathBuf::from("/certs.json"));
        assert_eq!(args.name, "Living Room");
        assert_eq!(args.model, "Chromecast");
        assert_eq!(args.bind_host, "0.0.0.0");
    }

    #[test]
    fn manifest_is_required() {
        assert!(Args::try_parse_from(["vibecast"]).is_err());
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
    fn one_app_is_registered() {
        assert_eq!(apps().len(), 1);
    }
}
