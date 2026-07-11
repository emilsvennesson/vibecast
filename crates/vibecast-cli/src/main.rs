//! vibecast receiver binary.
//!
//! The desktop platform binding. It sources settings (TOML + CLI overrides),
//! resolves the data dir and certificate path, then hands the shared
//! [`vibecast_platform`] compose logic a fully-typed config and runs until
//! Ctrl-C. Players register over the bridge and each is given its own Cast
//! receiver (advertised over mDNS from Rust). All assembly lives in
//! `vibecast-platform`, shared verbatim with the Android/iOS FFI binding.
//!
//! The `capture` subcommand is a desktop-only developer tool that MITMs the
//! Cast protocol and the receiver's HTTP/HTTPS egress for reverse-engineering
//! (see `vibecast-capture`).

use std::path::PathBuf;

use anyhow::Context;
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use vibecast_platform::{Config, PlatformInputs};

/// Command-line arguments. Flags override matching `config.toml` values.
#[derive(Debug, Parser)]
#[command(name = "vibecast", about = "A native Google Cast receiver")]
struct Args {
    /// Certificate manifest path (overrides `[device].certs`; relative paths
    /// resolve from the data dir).
    #[arg(long, global = true)]
    certs: Option<PathBuf>,

    /// Data directory holding `config.toml` and receiver state
    /// (default: `$HOME/.vibecast`).
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,

    /// Override the configured device model (reported by every player's receiver).
    #[arg(long, global = true)]
    model: Option<String>,

    /// Override the configured bind host.
    #[arg(long, global = true)]
    bind_host: Option<String>,

    /// Override the player-bridge port (players connect here to register).
    #[arg(long, global = true)]
    player_port: Option<u16>,

    /// Log level (`trace|debug|info|warn|error`); overrides `RUST_LOG`.
    #[arg(long, global = true)]
    log_level: Option<String>,

    #[command(subcommand)]
    command: Option<Command>,
}

/// Subcommands. With none, vibecast runs as a receiver (the default).
#[derive(Debug, Subcommand)]
enum Command {
    /// Capture Cast + HTTP/HTTPS traffic between a sender and a genuine
    /// receiver for reverse-engineering (developer tool; needs a rooted
    /// Android device reachable over adb for HTTP capture).
    Capture(CaptureArgs),
}

/// Arguments for `vibecast capture`.
#[derive(Debug, clap::Args)]
struct CaptureArgs {
    /// Genuine receiver IP/host to relay to (e.g. the real Chromecast/Shield).
    #[arg(long)]
    upstream: String,

    /// Genuine receiver CastV2 port.
    #[arg(long, default_value_t = 8009)]
    upstream_port: u16,

    /// Local port to listen on for senders.
    #[arg(long, default_value_t = 8009)]
    listen_port: u16,

    /// Output directory; the session is written to `<out>/<name>/`.
    #[arg(long, default_value = "captures")]
    out: PathBuf,

    /// Session name (default: a UTC timestamp).
    #[arg(long)]
    name: Option<String>,

    /// Target a specific adb device serial (for multi-device hosts).
    #[arg(long)]
    adb_serial: Option<String>,

    /// MITM CA (certificate + private key) PEM enabling HTTP/HTTPS capture,
    /// e.g. `~/.mitmproxy/mitmproxy-ca.pem`. The CA must already be trusted on
    /// the device (e.g. a Magisk cert module). Omit for Cast-only capture.
    #[arg(long)]
    ca: Option<PathBuf>,

    /// Capture Cast only; skip the HTTP/HTTPS MITM (no adb/device changes).
    #[arg(long)]
    no_http: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = Args::parse();
    init_tracing(args.log_level.as_deref());
    vibecast_platform::install_default_crypto_provider();

    match args.command.take() {
        Some(Command::Capture(capture)) => run_capture(args, capture).await,
        None => run(args).await,
    }
}

async fn run(args: Args) -> anyhow::Result<()> {
    let data_dir = args.data_dir.clone().unwrap_or_else(default_data_dir);
    std::fs::create_dir_all(&data_dir)
        .with_context(|| format!("creating data dir {}", data_dir.display()))?;

    let mut config = Config::load(&data_dir)?;

    // CLI flags override config.
    if let Some(model) = args.model.clone() {
        config.device.model = model;
    }
    if let Some(bind_host) = args.bind_host.clone() {
        config.network.bind_host = bind_host;
    }
    if let Some(player_port) = args.player_port {
        config.network.player_port = player_port;
    }

    let certs_path = resolve_certs_path(&args, &config, &data_dir);

    let inputs = PlatformInputs {
        data_dir,
        certs_path,
        advertise_mdns: true,
        // Desktop derives the reported LAN IP from the routed interface.
        local_ip: None,
    };

    let receiver = vibecast_platform::run(config, inputs, None)
        .await
        .context("starting receiver")?;

    tracing::info!(
        ip = %receiver.local_ip,
        register = format_args!("ws://{}:{}/player", receiver.local_ip, receiver.player_port),
        web = format_args!("http://{}:{}/", receiver.local_ip, receiver.player_port),
        "vibecast server started; waiting for players to register"
    );

    tokio::signal::ctrl_c()
        .await
        .context("waiting for shutdown signal")?;
    tracing::info!("shutting down");
    receiver.shutdown().await;
    Ok(())
}

async fn run_capture(args: Args, capture: CaptureArgs) -> anyhow::Result<()> {
    let data_dir = args.data_dir.clone().unwrap_or_else(default_data_dir);
    std::fs::create_dir_all(&data_dir)
        .with_context(|| format!("creating data dir {}", data_dir.display()))?;

    let config = Config::load(&data_dir)?;
    let certs_path = resolve_certs_path(&args, &config, &data_dir);
    let device_id = load_or_create_device_id(&data_dir.join("capture_proxy_device_id"))?;

    let model = args.model.clone().unwrap_or(config.device.model);
    let friendly_name = capture.upstream.clone();

    // HTTP/HTTPS capture requires a pre-trusted MITM CA (cert + key PEM).
    let ca = if capture.no_http {
        None
    } else if let Some(ca_path) = &capture.ca {
        Some(load_ca_material(ca_path)?)
    } else {
        tracing::warn!(
            "no --ca provided; capturing Cast only. Pass --ca <cert+key.pem> \
             (e.g. ~/.mitmproxy/mitmproxy-ca.pem) to also capture HTTP/HTTPS."
        );
        None
    };

    let cfg = vibecast_capture::CaptureConfig {
        certs_path,
        upstream_host: capture.upstream,
        upstream_port: capture.upstream_port,
        listen_port: capture.listen_port,
        out_dir: capture.out,
        name: capture.name,
        adb_serial: capture.adb_serial,
        ca,
        device_id,
        friendly_name,
        model,
    };

    vibecast_capture::run_capture(cfg)
        .await
        .context("running capture")?;
    Ok(())
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

/// Load a MITM CA from a PEM file containing both the certificate and the
/// private key (e.g. `~/.mitmproxy/mitmproxy-ca.pem`).
fn load_ca_material(path: &std::path::Path) -> anyhow::Result<vibecast_capture::CaMaterial> {
    let pem = std::fs::read_to_string(path)
        .with_context(|| format!("reading CA file {}", path.display()))?;

    let cert_pem = extract_pem_block(&pem, "CERTIFICATE")
        .with_context(|| format!("no CERTIFICATE block in {}", path.display()))?;
    let key_pem = extract_pem_block(&pem, "PRIVATE KEY")
        .with_context(|| format!("no PRIVATE KEY block in {}", path.display()))?;

    Ok(vibecast_capture::CaMaterial { cert_pem, key_pem })
}

/// Return the first PEM block whose header contains `kind` (e.g. `CERTIFICATE`,
/// `PRIVATE KEY` — also matches `RSA PRIVATE KEY`), including its markers.
fn extract_pem_block(pem: &str, kind: &str) -> Option<String> {
    let begin_marker = "-----BEGIN ";
    let mut rest = pem;
    while let Some(start) = rest.find(begin_marker) {
        let header_end = rest[start..].find('\n')? + start;
        let header = &rest[start + begin_marker.len()..header_end];
        if let Some(label) = header.strip_suffix("-----") {
            if label.contains(kind) {
                let end_marker = format!("-----END {label}-----");
                let end = rest[header_end..].find(&end_marker)? + header_end + end_marker.len();
                return Some(rest[start..end].to_string());
            }
        }
        rest = &rest[header_end..];
    }
    None
}

/// Read a persisted device id, or create and store a fresh one.
fn load_or_create_device_id(path: &std::path::Path) -> anyhow::Result<String> {
    if let Ok(existing) = std::fs::read_to_string(path) {
        let trimmed = existing.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }
    let id = uuid_like();
    std::fs::write(path, &id).with_context(|| format!("writing {}", path.display()))?;
    Ok(id)
}

/// A 32-hex-char id derived from the current time + process id. Persisted, so
/// it only needs to be unpredictable-enough to be a stable device identity.
fn uuid_like() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = u128::from(std::process::id());
    format!("{:016x}{:016x}", nanos, pid ^ nanos.rotate_left(17))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_optional_overrides() {
        let args =
            Args::try_parse_from(["vibecast", "--certs", "/certs.json", "--model", "Nest Hub"])
                .unwrap();
        assert_eq!(args.certs, Some(PathBuf::from("/certs.json")));
        assert_eq!(args.model.as_deref(), Some("Nest Hub"));
        assert_eq!(args.bind_host, None);
        assert!(args.command.is_none());
    }

    #[test]
    fn no_args_is_valid() {
        let args = Args::try_parse_from(["vibecast"]).unwrap();
        assert_eq!(args.certs, None);
        assert_eq!(args.model, None);
        assert!(args.command.is_none());
    }

    #[test]
    fn certs_path_falls_back_to_config_relative_to_data_dir() {
        let args = Args::try_parse_from(["vibecast"]).unwrap();
        let config = Config::default();
        let path = resolve_certs_path(&args, &config, std::path::Path::new("/data"));
        assert_eq!(path, PathBuf::from("/data/certs.json"));
    }

    #[test]
    fn parses_capture_subcommand() {
        let args = Args::try_parse_from([
            "vibecast",
            "capture",
            "--upstream",
            "192.168.2.6",
            "--name",
            "svt",
        ])
        .unwrap();
        match args.command {
            Some(Command::Capture(c)) => {
                assert_eq!(c.upstream, "192.168.2.6");
                assert_eq!(c.upstream_port, 8009);
                assert_eq!(c.name.as_deref(), Some("svt"));
                assert!(!c.no_http);
            }
            _ => panic!("expected capture subcommand"),
        }
    }

    #[test]
    fn capture_requires_upstream() {
        assert!(Args::try_parse_from(["vibecast", "capture"]).is_err());
    }

    #[test]
    fn extracts_cert_and_key_pem_blocks() {
        let pem = "prefix\n\
             -----BEGIN RSA PRIVATE KEY-----\nKEYDATA\n-----END RSA PRIVATE KEY-----\n\
             junk\n\
             -----BEGIN CERTIFICATE-----\nCERTDATA\n-----END CERTIFICATE-----\ntrailer\n";
        let cert = extract_pem_block(pem, "CERTIFICATE").unwrap();
        assert!(cert.starts_with("-----BEGIN CERTIFICATE-----"));
        assert!(cert.trim_end().ends_with("-----END CERTIFICATE-----"));
        assert!(cert.contains("CERTDATA"));

        let key = extract_pem_block(pem, "PRIVATE KEY").unwrap();
        assert!(key.contains("RSA PRIVATE KEY"));
        assert!(key.contains("KEYDATA"));

        assert!(extract_pem_block(pem, "DH PARAMETERS").is_none());
    }
}
