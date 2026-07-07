//! vibecast receiver binary.
//!
//! The desktop platform binding. It sources settings (TOML + CLI overrides),
//! resolves the data dir and certificate path, then hands the shared
//! [`vibecast_platform`] compose logic a fully-typed config and runs until
//! Ctrl-C. Players register over the bridge and each is given its own Cast
//! receiver (advertised over mDNS from Rust). All assembly lives in
//! `vibecast-platform`, shared verbatim with the Android/iOS FFI binding.

use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;
use tracing_subscriber::EnvFilter;

use vibecast_platform::{Config, PlatformInputs};

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

    /// Override the configured device model (reported by every player's receiver).
    #[arg(long)]
    model: Option<String>,

    /// Override the configured bind host.
    #[arg(long)]
    bind_host: Option<String>,

    /// Override the player-bridge port (players connect here to register).
    #[arg(long)]
    player_port: Option<u16>,

    /// Log level (`trace|debug|info|warn|error`); overrides `RUST_LOG`.
    #[arg(long)]
    log_level: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    init_tracing(args.log_level.as_deref());
    vibecast_platform::install_default_crypto_provider();
    run(args).await
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
    }

    #[test]
    fn no_args_is_valid() {
        let args = Args::try_parse_from(["vibecast"]).unwrap();
        assert_eq!(args.certs, None);
        assert_eq!(args.model, None);
    }

    #[test]
    fn certs_path_falls_back_to_config_relative_to_data_dir() {
        let args = Args::try_parse_from(["vibecast"]).unwrap();
        let config = Config::default();
        let path = resolve_certs_path(&args, &config, std::path::Path::new("/data"));
        assert_eq!(path, PathBuf::from("/data/certs.json"));
    }
}
