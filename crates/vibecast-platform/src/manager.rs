//! The per-player orchestrator.
//!
//! The [`PlayerManager`] consumes [`PlayerEvent`]s from the shared player bridge
//! and, for each player that registers, spins up a dedicated Cast receiver with
//! its own identity ("<name> [vibecast]"), fresh device id, dynamically-assigned
//! ports, and the player's reported capabilities. When a player disconnects its
//! receiver is torn down (ephemeral lifecycle). The manager also owns the single
//! certificate-rotation loop, hot-swapping every live receiver on rotation.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use vibecast_bridge::{PlayerBridge, PlayerEvent, PlayerRegistration};
use vibecast_cast::AuthMaterial;
use vibecast_core::{AppRegistry, DeviceIdentity};
use vibecast_discovery::{DeviceCapabilities, EurekaIdentity};
use vibecast_messages::Volume;
use vibecast_player_api::{Player, PlayerReport};
use vibecast_receiver::{
    spawn as spawn_receiver, ReceiverParams, RunningReceiver as ReceiverInstance,
};
use vibecast_security::{CertResolver, CertificateStore};

/// Facts about a player's receiver, reported to a frontend that owns discovery
/// (e.g. Android `NsdManager`) so it can advertise the service per player.
pub struct PlayerStarted {
    /// The player's stable id (from its registration).
    pub player_id: String,
    /// The advertised friendly name (`<reported name> [vibecast]`).
    pub name: String,
    /// mDNS service instance label.
    pub instance_name: String,
    /// CastV2 TLS port bound for this player's receiver.
    pub cast_port: u16,
    /// Eureka HTTP port bound for this player's receiver.
    pub eureka_http_port: u16,
    /// Cast TXT record key/value pairs.
    pub txt: Vec<(String, String)>,
}

/// Per-player lifecycle callbacks for a frontend that owns discovery
/// registration. Desktop advertises from Rust and can ignore these.
pub trait PlayerObserver: Send + Sync {
    /// A player's receiver started and (unless Rust advertises) needs registering.
    fn on_player_started(&self, _started: PlayerStarted) {}
    /// A player's advertised TXT record changed (certificate rotation).
    fn on_player_txt_changed(&self, _player_id: &str, _txt: Vec<(String, String)>) {}
    /// A player's receiver stopped and should be unregistered.
    fn on_player_stopped(&self, _player_id: &str) {}
}

/// Eureka identity fields shared across every per-player receiver.
pub(crate) struct EurekaConfig {
    pub manufacturer: String,
    pub locale: String,
    pub country_code: String,
    pub build_version: String,
    pub build_revision: String,
    pub capabilities: DeviceCapabilities,
}

/// Everything the manager needs to compose per-player receivers.
pub(crate) struct ManagerConfig {
    pub bridge: Arc<PlayerBridge>,
    pub registry: AppRegistry,
    pub discovery_app_ids: Vec<String>,
    pub http: reqwest::Client,
    pub data_dir: PathBuf,
    pub model: String,
    pub volume: Volume,
    pub user_agent: String,
    pub cast_device_capabilities: String,
    pub resolver: Arc<CertResolver>,
    pub store: CertificateStore,
    pub crl: Option<Vec<u8>>,
    pub bind_host: String,
    pub local_ip: String,
    pub eureka: EurekaConfig,
    pub advertise_mdns: bool,
    pub cert_rotation_poll: f64,
    pub observer: Option<Arc<dyn PlayerObserver>>,
}

pub(crate) struct PlayerManager {
    config: ManagerConfig,
    bundle: vibecast_security::CertificateBundle,
    receivers: HashMap<String, ReceiverInstance>,
}

impl PlayerManager {
    pub(crate) fn new(config: ManagerConfig, bundle: vibecast_security::CertificateBundle) -> Self {
        Self {
            config,
            bundle,
            receivers: HashMap::new(),
        }
    }

    /// Run the manager loop until `shutdown` is cancelled, then tear down all
    /// receivers.
    pub(crate) async fn run(
        mut self,
        mut events: mpsc::Receiver<PlayerEvent>,
        shutdown: CancellationToken,
    ) {
        let period = Duration::from_secs_f64(self.config.cert_rotation_poll.max(1.0));
        let mut ticker = tokio::time::interval(period);
        ticker.tick().await; // consume the immediate first tick

        loop {
            tokio::select! {
                () = shutdown.cancelled() => break,
                _ = ticker.tick() => self.rotate_certificates().await,
                event = events.recv() => match event {
                    Some(PlayerEvent::Registered { registration, player, reports }) => {
                        self.on_registered(*registration, player, reports).await;
                    }
                    Some(PlayerEvent::Disconnected { player_id }) => {
                        self.on_disconnected(&player_id).await;
                    }
                    None => break,
                },
            }
        }

        for (player_id, receiver) in self.receivers.drain() {
            receiver.shutdown().await;
            if let Some(observer) = &self.config.observer {
                observer.on_player_stopped(&player_id);
            }
        }
    }

    async fn on_registered(
        &mut self,
        registration: PlayerRegistration,
        player: Arc<dyn Player>,
        reports: mpsc::Receiver<PlayerReport>,
    ) {
        let player_id = registration.player_id.clone();

        // Reconnect / duplicate id: replace the previous receiver.
        if let Some(existing) = self.receivers.remove(&player_id) {
            existing.shutdown().await;
        }

        let device_id = uuid::Uuid::new_v4().to_string();
        let friendly_name = format!("{} [vibecast]", registration.name);
        let eureka_identity = self.eureka_identity(&friendly_name, &device_id);

        let params = ReceiverParams {
            identity: DeviceIdentity::new(
                friendly_name.clone(),
                self.config.model.clone(),
                device_id,
            ),
            eureka_identity,
            registry: self.config.registry.clone(),
            player,
            proxy: self.config.bridge.clone(),
            reports,
            http: self.config.http.clone(),
            data_dir: self.config.data_dir.clone(),
            volume: self.config.volume.clone(),
            user_agent: self.config.user_agent.clone(),
            cast_device_capabilities: self.config.cast_device_capabilities.clone(),
            capabilities: registration.capabilities,
            resolver: self.config.resolver.clone(),
            bundle: self.bundle.clone(),
            crl: self.config.crl.clone(),
            bind_host: self.config.bind_host.clone(),
            // Dynamic ports: each per-player receiver binds OS-assigned ports.
            cast_port: 0,
            eureka_http_port: 0,
            eureka_https_port: 0,
            advertise_mdns: self.config.advertise_mdns,
            app_ids: self.config.discovery_app_ids.clone(),
        };

        let receiver = match spawn_receiver(params).await {
            Ok(receiver) => receiver,
            Err(error) => {
                tracing::error!(%error, player_id = %player_id, "failed to start receiver for player");
                return;
            }
        };

        tracing::info!(
            player_id = %player_id,
            name = %friendly_name,
            cast_port = receiver.cast_port,
            instance = %receiver.instance_name,
            "started receiver for player"
        );

        if let Some(observer) = &self.config.observer {
            observer.on_player_started(PlayerStarted {
                player_id: player_id.clone(),
                name: friendly_name,
                instance_name: receiver.instance_name.clone(),
                cast_port: receiver.cast_port,
                eureka_http_port: receiver.eureka_http_port,
                txt: receiver.txt.clone(),
            });
        }

        self.receivers.insert(player_id, receiver);
    }

    async fn on_disconnected(&mut self, player_id: &str) {
        if let Some(receiver) = self.receivers.remove(player_id) {
            receiver.shutdown().await;
            tracing::info!(player_id = %player_id, "stopped receiver for player");
            if let Some(observer) = &self.config.observer {
                observer.on_player_stopped(player_id);
            }
        }
    }

    async fn rotate_certificates(&mut self) {
        let rotated = match self.config.store.rotate_if_needed(unix_now()) {
            Ok(Some(bundle)) => bundle.clone(),
            Ok(None) => return,
            Err(error) => {
                tracing::error!(%error, "certificate rotation check failed");
                return;
            }
        };
        if let Err(error) = self.config.resolver.update(&rotated) {
            tracing::error!(%error, "failed to hot-swap TLS certificate");
            return;
        }
        // New receivers registered after this point use the rotated bundle.
        self.bundle = rotated.clone();
        let crl = rotated.crl.clone().or_else(|| self.config.crl.clone());
        let digest = rotated.cert_digest_md5();
        for (player_id, receiver) in &self.receivers {
            receiver.rotation_handle().update_auth(AuthMaterial {
                bundle: rotated.clone(),
                crl: crl.clone(),
            });
            let txt = receiver.rotation_handle().update_cert_digest(&digest).await;
            if let Some(observer) = &self.config.observer {
                observer.on_player_txt_changed(player_id, txt);
            }
        }
        tracing::info!("rotated active certificate (TLS + device-auth + discovery)");
    }

    fn eureka_identity(&self, friendly_name: &str, device_id: &str) -> EurekaIdentity {
        let mut identity = EurekaIdentity::new(
            friendly_name.to_string(),
            self.config.model.clone(),
            device_id.to_string(),
            self.config.local_ip.clone(),
        );
        identity.manufacturer = self.config.eureka.manufacturer.clone();
        identity.locale = self.config.eureka.locale.clone();
        identity.country_code = self.config.eureka.country_code.clone();
        identity.build_version = self.config.eureka.build_version.clone();
        identity.build_revision = self.config.eureka.build_revision.clone();
        identity.capabilities = Some(self.config.eureka.capabilities.clone());
        identity
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
