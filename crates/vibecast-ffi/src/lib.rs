//! Platform-neutral UniFFI facade over the vibecast receiver.
//!
//! This is the sibling of `vibecast-cli`: same role (compose the portable
//! cores via [`vibecast_platform`] and own the lifecycle), different output —
//! a `cdylib` with UniFFI-generated Kotlin/Swift/... bindings instead of a
//! `main()`. The facade ([`ServerConfig`], [`ReceiverHandle`],
//! [`ReceiverObserver`], [`ReceiverError`]) is identical for every frontend;
//! each frontend supplies an observer and its own discovery registration,
//! foreground/lifecycle handling, storage paths, and permissions.
//!
//! Threading model: Rust owns one multi-threaded Tokio runtime for the handle's
//! lifetime. `start`/`stop` are **blocking** (the frontend calls them off its
//! main thread) and `block_on` the async bootstrap/teardown — deliberately
//! avoiding `async fn` across FFI (dodges UniFFI #2576 + JNA-async edge cases).
//! Foreign observer callbacks fire from Tokio worker threads; JNA attaches
//! those threads to the JVM when it invokes the callback.
//!
//! `#![forbid(unsafe_code)]` is intentionally **not** applied here: the whole
//! point of the crate is the FFI boundary, and `uniffi::setup_scaffolding!()`
//! emits `unsafe extern "C"` scaffolding. No hand-written `unsafe` appears in
//! this file; `#![deny(unsafe_code)]` below still forbids that (UniFFI's
//! generated code carries its own `#[allow(unsafe_code)]`).
#![deny(unsafe_code)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex, Once};

use vibecast_platform::{
    Config, PlatformError, PlatformInputs, PlayerObserver, PlayerStarted, RunningReceiver,
};

uniffi::setup_scaffolding!();

/// Receiver configuration supplied by the native frontend. Fields not present
/// here (manufacturer, locale, volume, timeouts) use the portable
/// Chromecast-like defaults from [`vibecast_platform::Config`].
///
/// Per-player Cast identities and their CastV2/eureka ports are assigned
/// dynamically as players register — they are not configured here.
#[derive(uniffi::Record)]
pub struct ServerConfig {
    /// Data directory (e.g. `Context.filesDir.absolutePath`).
    pub data_dir: String,
    /// Certificate manifest path (provisioned into the data dir).
    pub certs_path: String,
    /// Device model string (reported by every player's receiver).
    pub model: String,
    /// Host/interface to bind (e.g. `0.0.0.0`).
    pub bind_host: String,
    /// Player-bridge port where players connect to register (e.g. 8010).
    pub player_port: u16,
    /// LAN IP the receiver reports to senders (eureka `ip_address`). Supply
    /// the frontend's LAN address (Android: first site-local IPv4 resolved
    /// from `NetworkInterface` enumeration); `None` falls back to the
    /// routed-interface heuristic.
    pub local_ip: Option<String>,
    /// Per-app config as a JSON object string (`{"<app_key>": { ... }}`).
    pub apps_config_json: Option<String>,
}

/// A single Cast TXT record entry, mirrored into the frontend's discovery
/// registration (Android `NsdServiceInfo.setAttribute`, iOS TXT record, ...).
#[derive(uniffi::Record)]
pub struct TxtEntry {
    /// TXT key (on-wire short name, e.g. `md`, `fn`, `cd`).
    pub key: String,
    /// TXT value.
    pub value: String,
}

/// Discovery facts for one newly started per-player receiver.
#[derive(uniffi::Record)]
pub struct PlayerStartedInfo {
    /// Stable player lifecycle key.
    pub player_id: String,
    /// Advertised friendly name.
    pub name: String,
    /// DNS-SD service instance name.
    pub instance_name: String,
    /// CastV2 TLS port.
    pub cast_port: u16,
    /// Eureka HTTP port.
    pub eureka_http_port: u16,
    /// Cast TXT record entries.
    pub txt: Vec<TxtEntry>,
}

/// Errors starting the receiver, surfaced as typed exceptions to the frontend.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum ReceiverError {
    /// The receiver is already started.
    #[error("receiver already running")]
    AlreadyRunning,
    /// A listening socket could not be bound (port in use / not permitted).
    #[error("failed to bind port {port}")]
    BindFailed {
        /// The port that could not be bound.
        port: u16,
    },
    /// Certificate material or TLS setup failed.
    // Field is `reason`, not `message`: UniFFI maps error variants to Throwable
    // subclasses in Kotlin, where a `message` field collides with Throwable.message.
    #[error("certificate/TLS error: {reason}")]
    Certs {
        /// Human-readable cause chain.
        reason: String,
    },
    /// The supplied configuration was invalid.
    #[error("configuration error: {reason}")]
    Config {
        /// Human-readable cause chain.
        reason: String,
    },
    /// Any other startup failure.
    #[error("receiver error: {reason}")]
    Other {
        /// Human-readable cause chain.
        reason: String,
    },
}

/// Error a foreign observer may raise; the Rust side logs and continues.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum CallbackError {
    /// The foreign observer implementation failed.
    #[error("observer callback failed: {reason}")]
    Failed {
        /// Failure detail.
        reason: String,
    },
}

/// Rust → native events. The frontend implements this to register each player's
/// Cast receiver over discovery (Android `NsdManager`), re-register on
/// certificate rotation, and drive its UI/service state.
///
/// Every callback is per-player: one physical player (browser / Kodi) that
/// registers over the bridge becomes one advertised Cast device.
#[uniffi::export(with_foreign)]
pub trait ReceiverObserver: Send + Sync {
    /// A player registered and its receiver bound ports; register it for
    /// discovery under `name` (already suffixed `... [vibecast]`).
    fn on_player_started(&self, started: PlayerStartedInfo) -> Result<(), CallbackError>;
    /// A player's advertised TXT record changed (certificate rotation).
    fn on_player_txt_changed(
        &self,
        player_id: String,
        txt: Vec<TxtEntry>,
    ) -> Result<(), CallbackError>;
    /// A player disconnected; unregister its receiver from discovery.
    fn on_player_stopped(&self, player_id: String) -> Result<(), CallbackError>;
    /// A non-fatal error occurred after startup.
    fn on_error(&self, message: String) -> Result<(), CallbackError>;
}

/// Adapts a foreign [`ReceiverObserver`] to the platform [`PlayerObserver`].
struct ForeignObserver {
    inner: Arc<dyn ReceiverObserver>,
}

impl PlayerObserver for ForeignObserver {
    fn on_player_started(&self, started: PlayerStarted) {
        let started = PlayerStartedInfo {
            player_id: started.player_id,
            name: started.name,
            instance_name: started.instance_name,
            cast_port: started.cast_port,
            eureka_http_port: started.eureka_http_port,
            txt: to_txt_entries(started.txt),
        };
        if let Err(error) = self.inner.on_player_started(started) {
            tracing::warn!(%error, "observer.on_player_started failed");
        }
    }

    fn on_player_txt_changed(&self, player_id: &str, txt: Vec<(String, String)>) {
        if let Err(error) = self
            .inner
            .on_player_txt_changed(player_id.to_string(), to_txt_entries(txt))
        {
            tracing::warn!(%error, "observer.on_player_txt_changed failed");
        }
    }

    fn on_player_stopped(&self, player_id: &str) {
        if let Err(error) = self.inner.on_player_stopped(player_id.to_string()) {
            tracing::warn!(%error, "observer.on_player_stopped failed");
        }
    }
}

/// Handle to a receiver instance. Owns the Tokio runtime and the running
/// receiver; `destroy()` (generated) releases it.
#[derive(uniffi::Object)]
pub struct ReceiverHandle {
    runtime: tokio::runtime::Runtime,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    running: Option<RunningReceiver>,
    observer: Option<Arc<dyn ReceiverObserver>>,
}

#[uniffi::export]
impl ReceiverHandle {
    /// Create a handle: init the platform log layer + rustls provider once,
    /// then build the owned multi-threaded Tokio runtime.
    #[uniffi::constructor]
    fn new() -> Arc<Self> {
        init_logging();
        vibecast_platform::install_default_crypto_provider();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("vibecast-rt")
            .build()
            .expect("failed to build Tokio runtime");
        Arc::new(Self {
            runtime,
            state: Mutex::new(State::default()),
        })
    }

    /// Start the receiver (blocking). Notifies `observer.on_started` on success.
    fn start(
        &self,
        config: ServerConfig,
        observer: Arc<dyn ReceiverObserver>,
    ) -> Result<(), ReceiverError> {
        let mut state = self.state.lock().expect("state mutex poisoned");
        if state.running.is_some() {
            return Err(ReceiverError::AlreadyRunning);
        }

        let (platform_config, inputs) = build_config(config)?;

        // Forward per-player lifecycle to the foreign observer.
        let player_observer: Arc<dyn PlayerObserver> = Arc::new(ForeignObserver {
            inner: observer.clone(),
        });

        let running = self.runtime.block_on(vibecast_platform::run(
            platform_config,
            inputs,
            Some(player_observer),
        ))?;

        state.observer = Some(observer);
        state.running = Some(running);
        Ok(())
    }

    /// Stop the server (blocking). Shutting down drains every per-player
    /// receiver, so the observer receives an `on_player_stopped` for each. A
    /// no-op if not running.
    fn stop(&self) {
        let running = {
            let mut state = self.state.lock().expect("state mutex poisoned");
            state.observer.take();
            state.running.take()
        };
        if let Some(running) = running {
            self.runtime.block_on(running.shutdown());
        }
    }

    /// Whether the receiver is currently running.
    fn is_running(&self) -> bool {
        self.state
            .lock()
            .expect("state mutex poisoned")
            .running
            .is_some()
    }
}

/// Map a [`ServerConfig`] onto the shared [`Config`] + [`PlatformInputs`].
fn build_config(config: ServerConfig) -> Result<(Config, PlatformInputs), ReceiverError> {
    let mut platform_config = Config::default();
    platform_config.device.model = config.model;
    platform_config.network.bind_host = config.bind_host;
    platform_config.network.player_port = config.player_port;

    if let Some(json) = config.apps_config_json {
        platform_config.apps =
            serde_json::from_str(&json).map_err(|error| ReceiverError::Config {
                reason: format!("apps_config_json: {error}"),
            })?;
    }

    let inputs = PlatformInputs {
        data_dir: PathBuf::from(config.data_dir),
        certs_path: PathBuf::from(config.certs_path),
        // Android/iOS advertise each player's receiver via the native discovery
        // API using the per-player facts from `on_player_started` — never via
        // mDNS from Rust.
        advertise_mdns: false,
        local_ip: config.local_ip.filter(|ip| !ip.trim().is_empty()),
    };
    Ok((platform_config, inputs))
}

fn to_txt_entries(pairs: Vec<(String, String)>) -> Vec<TxtEntry> {
    pairs
        .into_iter()
        .map(|(key, value)| TxtEntry { key, value })
        .collect()
}

impl From<PlatformError> for ReceiverError {
    fn from(error: PlatformError) -> Self {
        let message = error_chain(&error);
        match error {
            PlatformError::Bind { port, .. } => ReceiverError::BindFailed { port },
            PlatformError::Certs(_) | PlatformError::InvalidHeader(_) => {
                ReceiverError::Certs { reason: message }
            }
            PlatformError::AppConfig { .. } | PlatformError::Registry(_) => {
                ReceiverError::Config { reason: message }
            }
            PlatformError::Discovery(_)
            | PlatformError::HttpClient(_)
            | PlatformError::BridgeStart(_)
            | PlatformError::StateRead { .. }
            | PlatformError::InvalidInstallationId { .. }
            | PlatformError::StateWrite { .. } => ReceiverError::Other { reason: message },
        }
    }
}

/// Render an error and its full `source()` chain into one readable line.
fn error_chain(error: &dyn std::error::Error) -> String {
    let mut message = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
}

static LOG_INIT: Once = Once::new();

/// Initialize the tracing sink once: logcat on Android, `fmt` elsewhere.
fn init_logging() {
    LOG_INIT.call_once(|| {
        #[cfg(target_os = "android")]
        {
            use tracing_logcat::{LogcatMakeWriter, LogcatTag};
            use tracing_subscriber::fmt::format::Format;

            if let Ok(writer) = LogcatMakeWriter::new(LogcatTag::Fixed("vibecast".to_owned())) {
                let _ = tracing_subscriber::fmt()
                    .event_format(Format::default().with_level(true).without_time())
                    .with_writer(writer)
                    .with_ansi(false)
                    .try_init();
            }
        }
        #[cfg(not(target_os = "android"))]
        {
            use tracing_subscriber::EnvFilter;
            let filter =
                EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
            let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_config() -> ServerConfig {
        ServerConfig {
            data_dir: "/tmp/vibecast-ffi-test".to_owned(),
            certs_path: "/tmp/vibecast-ffi-test/certs.json".to_owned(),
            model: "Chromecast".to_owned(),
            bind_host: "127.0.0.1".to_owned(),
            player_port: 9010,
            local_ip: None,
            apps_config_json: None,
        }
    }

    #[test]
    fn build_config_maps_fields_and_forces_no_mdns() {
        let (config, inputs) = build_config(base_config()).unwrap();
        assert_eq!(config.device.model, "Chromecast");
        assert_eq!(config.network.player_port, 9010);
        assert!(!inputs.advertise_mdns);
        assert_eq!(inputs.local_ip, None);
        let _ = inputs.certs_path;
    }

    #[test]
    fn build_config_threads_local_ip_and_treats_blank_as_absent() {
        let mut sc = base_config();
        sc.local_ip = Some("192.168.1.42".to_owned());
        let (_, inputs) = build_config(sc).unwrap();
        assert_eq!(inputs.local_ip.as_deref(), Some("192.168.1.42"));

        let mut sc = base_config();
        sc.local_ip = Some("   ".to_owned());
        let (_, inputs) = build_config(sc).unwrap();
        assert_eq!(inputs.local_ip, None);
    }

    #[test]
    fn build_config_parses_apps_json() {
        let mut sc = base_config();
        sc.apps_config_json = Some(r#"{"primevideo":{"marketplace_id":"X"}}"#.to_owned());
        let (config, _) = build_config(sc).unwrap();
        assert_eq!(config.apps["primevideo"]["marketplace_id"], "X");
    }

    #[test]
    fn build_config_rejects_bad_apps_json() {
        let mut sc = base_config();
        sc.apps_config_json = Some("not json".to_owned());
        assert!(matches!(
            build_config(sc),
            Err(ReceiverError::Config { .. })
        ));
    }

    #[test]
    fn platform_bind_error_maps_to_bind_failed() {
        let err = PlatformError::Bind {
            what: "cast",
            port: 9009,
            source: std::io::Error::new(std::io::ErrorKind::AddrInUse, "in use"),
        };
        assert!(matches!(
            ReceiverError::from(err),
            ReceiverError::BindFailed { port: 9009 }
        ));
    }
}
