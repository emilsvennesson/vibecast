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

use vibecast_platform::{Config, PlatformError, PlatformInputs, RunningReceiver, TxtObserver};

uniffi::setup_scaffolding!();

/// Receiver configuration supplied by the native frontend. Fields not present
/// here (manufacturer, locale, per-cast identity, volume, timeouts) use the
/// portable Chromecast-like defaults from [`vibecast_platform::Config`].
#[derive(uniffi::Record)]
pub struct ServerConfig {
    /// Data directory (e.g. `Context.filesDir.absolutePath`).
    pub data_dir: String,
    /// Certificate manifest path (provisioned into the data dir).
    pub certs_path: String,
    /// Friendly name advertised to senders.
    pub friendly_name: String,
    /// Device model string.
    pub model: String,
    /// Host/interface to bind (e.g. `0.0.0.0`).
    pub bind_host: String,
    /// CastV2 TLS port (use an alternate port to coexist with a built-in
    /// Cast receiver, e.g. 9009).
    pub cast_port: u16,
    /// Eureka HTTP port (alternate, e.g. 9008).
    pub eureka_http_port: u16,
    /// Eureka HTTPS port (alternate, e.g. 9443).
    pub eureka_https_port: u16,
    /// Player-bridge port (loopback, e.g. 8010).
    pub player_port: u16,
    /// Stable device id. When absent, one is loaded/created under `data_dir`.
    pub device_id: Option<String>,
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

/// Rust → native events. The frontend implements this to register discovery,
/// re-register on certificate rotation, and drive its UI/service state.
#[uniffi::export(with_foreign)]
pub trait ReceiverObserver: Send + Sync {
    /// The receiver bound its ports and is ready to be advertised.
    fn on_started(
        &self,
        cast_port: u16,
        eureka_http_port: u16,
        instance_name: String,
        txt: Vec<TxtEntry>,
    ) -> Result<(), CallbackError>;
    /// The advertised TXT record changed (certificate rotation) — re-register.
    fn on_txt_changed(&self, txt: Vec<TxtEntry>) -> Result<(), CallbackError>;
    /// The receiver stopped cleanly.
    fn on_stopped(&self) -> Result<(), CallbackError>;
    /// A non-fatal error occurred after startup.
    fn on_error(&self, message: String) -> Result<(), CallbackError>;
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

        // Forward TXT-record changes (cert rotation) to the foreign observer.
        let txt_observer = observer.clone();
        let on_txt: TxtObserver = Arc::new(move |pairs: Vec<(String, String)>| {
            if let Err(error) = txt_observer.on_txt_changed(to_txt_entries(pairs)) {
                tracing::warn!(%error, "observer.on_txt_changed failed");
            }
        });

        let running = self.runtime.block_on(vibecast_platform::run(
            platform_config,
            inputs,
            Some(on_txt),
        ))?;

        // Capture the facts the observer needs, store state, then release the
        // lock *before* invoking the foreign callback: `state` is a non-reentrant
        // `std::sync::Mutex`, so an observer that calls back into the handle
        // (e.g. `is_running`/`stop`) from `on_started` would otherwise deadlock.
        let started = (
            running.cast_port,
            running.eureka_http_port,
            running.instance_name.clone(),
            to_txt_entries(running.txt.clone()),
        );
        state.observer = Some(observer.clone());
        state.running = Some(running);
        drop(state);

        let (cast_port, eureka_http_port, instance_name, txt) = started;
        if let Err(error) = observer.on_started(cast_port, eureka_http_port, instance_name, txt) {
            tracing::warn!(%error, "observer.on_started failed");
        }
        Ok(())
    }

    /// Stop the receiver (blocking) and notify `observer.on_stopped`. A no-op
    /// if not running.
    fn stop(&self) {
        let (running, observer) = {
            let mut state = self.state.lock().expect("state mutex poisoned");
            (state.running.take(), state.observer.take())
        };
        if let Some(running) = running {
            self.runtime.block_on(running.shutdown());
        }
        if let Some(observer) = observer {
            if let Err(error) = observer.on_stopped() {
                tracing::warn!(%error, "observer.on_stopped failed");
            }
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
    platform_config.device.friendly_name = config.friendly_name;
    platform_config.device.model = config.model;
    platform_config.network.bind_host = config.bind_host;
    platform_config.network.cast_port = config.cast_port;
    platform_config.network.eureka_http_port = config.eureka_http_port;
    platform_config.network.eureka_https_port = config.eureka_https_port;
    platform_config.network.player_port = config.player_port;

    if let Some(json) = config.apps_config_json {
        platform_config.apps =
            serde_json::from_str(&json).map_err(|error| ReceiverError::Config {
                reason: format!("apps_config_json: {error}"),
            })?;
    }

    let data_dir = PathBuf::from(config.data_dir);
    let certs_path = PathBuf::from(config.certs_path);
    let device_id = config
        .device_id
        .unwrap_or_else(|| vibecast_platform::load_or_create_device_id(&data_dir));

    let inputs = PlatformInputs {
        data_dir,
        certs_path,
        device_id,
        // Android/iOS advertise via the native discovery API using the
        // returned instance/txt — never via mDNS from Rust.
        advertise_mdns: false,
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
            | PlatformError::BridgeStart(_) => ReceiverError::Other { reason: message },
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
            friendly_name: "Test Room".to_owned(),
            model: "Chromecast".to_owned(),
            bind_host: "127.0.0.1".to_owned(),
            cast_port: 9009,
            eureka_http_port: 9008,
            eureka_https_port: 9443,
            player_port: 9010,
            device_id: Some("test-device".to_owned()),
            apps_config_json: None,
        }
    }

    #[test]
    fn build_config_maps_fields_and_forces_no_mdns() {
        let (config, inputs) = build_config(base_config()).unwrap();
        assert_eq!(config.device.friendly_name, "Test Room");
        assert_eq!(config.network.cast_port, 9009);
        assert_eq!(config.network.player_port, 9010);
        assert_eq!(inputs.device_id, "test-device");
        assert!(!inputs.advertise_mdns);
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
