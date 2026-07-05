//! vibecast receiver binary.
//!
//! The desktop platform binding. It sources settings (TOML + CLI overrides),
//! resolves the data dir, certificate path, and device id, then hands the
//! shared [`vibecast_platform`] compose logic a fully-typed config and runs
//! until Ctrl-C. All receiver assembly lives in `vibecast-platform`, shared
//! verbatim with the Android/iOS FFI binding.

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
    if let Some(name) = args.name.clone() {
        config.device.friendly_name = name;
    }
    if let Some(model) = args.model.clone() {
        config.device.model = model;
    }
    if let Some(bind_host) = args.bind_host.clone() {
        config.network.bind_host = bind_host;
    }
    if let Some(cast_port) = args.cast_port {
        config.network.cast_port = cast_port;
    }

    let certs_path = resolve_certs_path(&args, &config, &data_dir);
    let device_id = match &args.device_id {
        Some(id) => id.clone(),
        None => vibecast_platform::load_or_create_device_id(&data_dir),
    };

    let inputs = PlatformInputs {
        data_dir,
        certs_path,
        device_id,
        advertise_mdns: true,
    };

    let receiver = vibecast_platform::run(config, inputs, None)
        .await
        .context("starting receiver")?;

    tracing::info!(
        ip = %receiver.local_ip,
        cast_port = receiver.cast_port,
        player = format_args!("http://{}:{}/", receiver.local_ip, receiver.player_port),
        "vibecast receiver started"
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
}
