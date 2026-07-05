//! `/setup/eureka_info` endpoints probed by Cast senders during discovery.
//!
//! The JSON payload mirrors a real Chromecast. The same handler serves HTTP
//! (8008) and HTTPS (8443); the payload is byte-for-byte identical modulo key
//! ordering, which senders don't depend on.

use std::sync::Arc;
use std::time::Instant;

use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use base64::prelude::{Engine, BASE64_STANDARD};
use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::Sha256;

use vibecast_security::CertificateBundle;

use crate::error::DiscoveryError;

/// Device capability flags advertised in eureka `device_info.capabilities`.
///
/// Also deserialized directly from the `[device.capabilities]` config table
/// (missing keys fall back to the Chromecast-like defaults).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[allow(missing_docs)]
pub struct DeviceCapabilities {
    pub audio_hdr_supported: bool,
    pub audio_surround_mode_supported: bool,
    pub cast_connect_supported: bool,
    pub cloud_groups_supported: bool,
    pub cloudcast_supported: bool,
    pub display_supported: bool,
    pub fdr_supported: bool,
    pub hdmi_prefer_50hz_supported: bool,
    pub hdmi_prefer_high_fps_supported: bool,
    pub hotspot_supported: bool,
    pub https_setup_supported: bool,
    pub keep_hotspot_until_connected_supported: bool,
    pub multizone_supported: bool,
    pub opencast_supported: bool,
    pub reboot_supported: bool,
    pub renaming_supported: bool,
    pub set_group_audio_delay_supported: bool,
    pub set_network_supported: bool,
    pub setup_supported: bool,
    pub stats_supported: bool,
    pub system_sound_effects_supported: bool,
    pub wifi_auto_save_supported: bool,
    pub wifi_supported: bool,
}

impl Default for DeviceCapabilities {
    fn default() -> Self {
        Self {
            audio_hdr_supported: false,
            audio_surround_mode_supported: false,
            cast_connect_supported: true,
            cloud_groups_supported: false,
            cloudcast_supported: true,
            display_supported: true,
            fdr_supported: false,
            hdmi_prefer_50hz_supported: false,
            hdmi_prefer_high_fps_supported: false,
            hotspot_supported: false,
            https_setup_supported: true,
            keep_hotspot_until_connected_supported: false,
            multizone_supported: true,
            opencast_supported: false,
            reboot_supported: false,
            renaming_supported: false,
            set_group_audio_delay_supported: false,
            set_network_supported: false,
            setup_supported: false,
            stats_supported: false,
            system_sound_effects_supported: false,
            wifi_auto_save_supported: false,
            wifi_supported: false,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct Location {
    country_code: String,
    latitude: f64,
    longitude: f64,
}

#[derive(Debug, Clone, Default, Serialize)]
struct OptIn {
    crash: bool,
    opencast: bool,
    stats: bool,
}

#[derive(Debug, Clone, Serialize)]
struct SetupStats {
    historically_succeeded: bool,
    num_check_connectivity: u32,
    num_connect_wifi: u32,
    num_connected_wifi_not_saved: u32,
    num_initial_eureka_info: u32,
    num_obtain_ip: u32,
}

impl Default for SetupStats {
    fn default() -> Self {
        Self {
            historically_succeeded: true,
            num_check_connectivity: 0,
            num_connect_wifi: 0,
            num_connected_wifi_not_saved: 0,
            num_initial_eureka_info: 0,
            num_obtain_ip: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct DeviceInfo {
    capabilities: DeviceCapabilities,
    cloud_device_id: String,
    factory_country_code: String,
    hotspot_bssid: String,
    local_authorization_token_hash: String,
    mac_address: String,
    manufacturer: String,
    model_name: String,
    product_name: String,
    public_key: String,
    ssdp_udn: String,
    uptime: f64,
    weave_device_id: String,
}

#[derive(Debug, Clone, Serialize)]
struct Multizone {
    audio_output_delay: f64,
    audio_output_delay_hdmi: f64,
    audio_output_delay_oem: f64,
    dynamic_groups: Vec<Value>,
    groups: Vec<Value>,
    max_static_groups: u32,
    multichannel_status: u32,
}

impl Default for Multizone {
    fn default() -> Self {
        Self {
            audio_output_delay: 0.0,
            audio_output_delay_hdmi: 0.0,
            audio_output_delay_oem: 0.0,
            dynamic_groups: Vec::new(),
            groups: Vec::new(),
            max_static_groups: 100,
            multichannel_status: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct EurekaInfo {
    bssid: String,
    build_version: String,
    cast_build_revision: String,
    connected: bool,
    ethernet_connected: bool,
    has_update: bool,
    hotspot_bssid: String,
    ip_address: String,
    locale: String,
    location: Location,
    mac_address: String,
    name: String,
    opt_in: OptIn,
    public_key: String,
    release_track: String,
    setup_state: u32,
    setup_stats: SetupStats,
    ssdp_udn: String,
    ssid: String,
    time_format: u32,
    tos_accepted: bool,
    uptime: f64,
    version: u32,
    wpa_configured: bool,
    wpa_state: u32,
    device_info: DeviceInfo,
    multizone: Multizone,
}

/// Static and per-device identity for the eureka payload.
#[derive(Debug, Clone)]
pub struct EurekaIdentity {
    /// Friendly device name.
    pub friendly_name: String,
    /// Device model string.
    pub device_model: String,
    /// SSDP UDN (also used for cloud id and token hash).
    pub ssdp_udn: String,
    /// The device's advertised IP address.
    pub ip_address: String,
    /// Manufacturer (default "Google Inc.").
    pub manufacturer: String,
    /// Locale (default "en-US").
    pub locale: String,
    /// Country code (default "US").
    pub country_code: String,
    /// Build version string.
    pub build_version: String,
    /// Cast build revision string.
    pub build_revision: String,
    /// Optional capability overrides.
    pub capabilities: Option<DeviceCapabilities>,
}

impl EurekaIdentity {
    /// Build an identity with Cast-default fields.
    #[must_use]
    pub fn new(
        friendly_name: String,
        device_model: String,
        ssdp_udn: String,
        ip_address: String,
    ) -> Self {
        Self {
            friendly_name,
            device_model,
            ssdp_udn,
            ip_address,
            manufacturer: "Google Inc.".into(),
            locale: "en-US".into(),
            country_code: "US".into(),
            build_version: "446070".into(),
            build_revision: "3.72.446070".into(),
            capabilities: None,
        }
    }
}

struct EurekaState {
    identity: EurekaIdentity,
    public_key_b64: String,
    token_hash: String,
    started: Instant,
}

/// Serves the eureka discovery endpoint over HTTP and/or HTTPS.
pub struct EurekaServer {
    state: Arc<EurekaState>,
}

impl EurekaServer {
    /// Build the server from certificate material (for the device public key)
    /// and identity.
    pub fn new(
        bundle: &CertificateBundle,
        identity: EurekaIdentity,
    ) -> Result<Self, DiscoveryError> {
        let public_key_b64 = BASE64_STANDARD.encode(bundle.device_public_key_der()?);
        let token_hash = token_hash(&identity.ssdp_udn);
        Ok(Self {
            state: Arc::new(EurekaState {
                identity,
                public_key_b64,
                token_hash,
                started: Instant::now(),
            }),
        })
    }

    /// The axum router exposing `/setup/eureka_info`.
    pub fn router(&self) -> Router {
        Router::new()
            .route("/setup/eureka_info", get(handle_eureka_info))
            .with_state(Arc::clone(&self.state))
    }

    /// Serve HTTP on the given listener until it errors.
    pub async fn serve_http(
        &self,
        listener: tokio::net::TcpListener,
    ) -> Result<(), DiscoveryError> {
        axum::serve(listener, self.router()).await?;
        Ok(())
    }

    /// Serve HTTPS on the given std listener using `config` for TLS.
    pub async fn serve_https(
        &self,
        listener: std::net::TcpListener,
        config: rustls::ServerConfig,
    ) -> Result<(), DiscoveryError> {
        let tls = axum_server::tls_rustls::RustlsConfig::from_config(Arc::new(config));
        axum_server::from_tcp_rustls(listener, tls)?
            .serve(self.router().into_make_service())
            .await?;
        Ok(())
    }
}

#[derive(Deserialize)]
struct ParamsQuery {
    params: Option<String>,
}

async fn handle_eureka_info(
    State(state): State<Arc<EurekaState>>,
    Query(query): Query<ParamsQuery>,
) -> Json<Value> {
    let params = query.params.as_deref().and_then(parse_params);
    Json(build_payload(&state, params.as_deref()))
}

fn parse_params(raw: &str) -> Option<Vec<String>> {
    let values: Vec<String> = raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    (!values.is_empty()).then_some(values)
}

fn build_payload(state: &EurekaState, params: Option<&[String]>) -> Value {
    let uptime = state.started.elapsed().as_secs_f64();
    let id = &state.identity;
    let product_name = product_name(&id.device_model);
    let cloud_device_id = cloud_device_id(&id.ssdp_udn);

    let info = EurekaInfo {
        bssid: String::new(),
        build_version: id.build_version.clone(),
        cast_build_revision: id.build_revision.clone(),
        connected: true,
        ethernet_connected: true,
        has_update: false,
        hotspot_bssid: String::new(),
        ip_address: id.ip_address.clone(),
        locale: id.locale.clone(),
        location: Location {
            country_code: id.country_code.clone(),
            latitude: 255.0,
            longitude: 255.0,
        },
        mac_address: "00:00:00:00:00:00".into(),
        name: id.friendly_name.clone(),
        opt_in: OptIn::default(),
        public_key: state.public_key_b64.clone(),
        release_track: String::new(),
        setup_state: 60,
        setup_stats: SetupStats::default(),
        ssdp_udn: id.ssdp_udn.clone(),
        ssid: String::new(),
        time_format: 1,
        tos_accepted: true,
        uptime,
        version: 12,
        wpa_configured: false,
        wpa_state: 0,
        device_info: DeviceInfo {
            capabilities: id.capabilities.clone().unwrap_or_default(),
            cloud_device_id,
            factory_country_code: String::new(),
            hotspot_bssid: String::new(),
            local_authorization_token_hash: state.token_hash.clone(),
            mac_address: "00:00:00:00:00:00".into(),
            manufacturer: id.manufacturer.clone(),
            model_name: id.device_model.clone(),
            product_name,
            public_key: state.public_key_b64.clone(),
            ssdp_udn: id.ssdp_udn.clone(),
            uptime,
            weave_device_id: String::new(),
        },
        multizone: Multizone::default(),
    };

    let mut value = serde_json::to_value(&info).expect("eureka info serializes");
    let object = value.as_object_mut().expect("eureka info is an object");

    match params {
        // Unfiltered response omits the optional blocks, like a real device.
        None => {
            object.remove("device_info");
            object.remove("multizone");
            value
        }
        Some(keys) => {
            let filtered: Map<String, Value> = keys
                .iter()
                .filter_map(|key| object.get(key).map(|v| (key.clone(), v.clone())))
                .collect();
            Value::Object(filtered)
        }
    }
}

fn cloud_device_id(ssdp_udn: &str) -> String {
    let cleaned: String = ssdp_udn.replace('-', "").to_uppercase();
    if cleaned.len() == 32 && cleaned.chars().all(|c| c.is_ascii_alphanumeric()) {
        cleaned
    } else {
        hex::encode_upper(Md5::digest(ssdp_udn.as_bytes()))
    }
}

fn product_name(model_name: &str) -> String {
    let normalized: String = model_name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    if normalized.is_empty() {
        "chromecast".into()
    } else {
        normalized
    }
}

fn token_hash(ssdp_udn: &str) -> String {
    BASE64_STANDARD.encode(Sha256::digest(ssdp_udn.as_bytes()))
}

#[cfg(test)]
mod tests {
    use axum::body::{to_bytes, Body};
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use super::*;

    fn bundle() -> CertificateBundle {
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = rcgen::CertificateParams::new(vec!["Device".to_string()])
            .unwrap()
            .self_signed(&key)
            .unwrap();
        CertificateBundle {
            peer_cert_pem: Vec::new(),
            peer_key_pem: Vec::new(),
            peer_cert_der: cert.der().to_vec(),
            device_cert_der: cert.der().to_vec(),
            intermediate_certs_der: Vec::new(),
            signature_sha1: Vec::new(),
            signature_sha256: Vec::new(),
            not_valid_before: 0,
            not_valid_after: i64::MAX,
            crl: None,
        }
    }

    fn server() -> EurekaServer {
        let identity = EurekaIdentity::new(
            "Living Room".into(),
            "Chromecast".into(),
            "12345678-1234-1234-1234-123456789abc".into(),
            "192.168.1.42".into(),
        );
        EurekaServer::new(&bundle(), identity).unwrap()
    }

    async fn get(uri: &str) -> Value {
        let response = server()
            .router()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn unfiltered_response_omits_optional_blocks() {
        let body = get("/setup/eureka_info").await;
        assert_eq!(body["name"], "Living Room");
        assert_eq!(body["ip_address"], "192.168.1.42");
        assert_eq!(body["version"], 12);
        assert_eq!(body["build_version"], "446070");
        assert_eq!(body["ssdp_udn"], "12345678-1234-1234-1234-123456789abc");
        assert!(!body["public_key"].as_str().unwrap().is_empty());
        // Optional blocks are omitted unless requested.
        assert!(body.get("device_info").is_none());
        assert!(body.get("multizone").is_none());
    }

    #[tokio::test]
    async fn params_filters_to_requested_keys_including_device_info() {
        let body = get("/setup/eureka_info?params=name,version,device_info,multizone").await;
        let object = body.as_object().unwrap();
        assert_eq!(object.len(), 4);
        assert_eq!(body["name"], "Living Room");
        assert_eq!(body["version"], 12);

        let device_info = &body["device_info"];
        assert_eq!(device_info["model_name"], "Chromecast");
        assert_eq!(device_info["product_name"], "chromecast");
        assert_eq!(device_info["manufacturer"], "Google Inc.");
        // token hash is base64 of sha256(ssdp_udn)
        assert!(!device_info["local_authorization_token_hash"]
            .as_str()
            .unwrap()
            .is_empty());
        assert_eq!(device_info["capabilities"]["cast_connect_supported"], true);

        assert_eq!(body["multizone"]["max_static_groups"], 100);
    }

    #[tokio::test]
    async fn unknown_params_are_ignored() {
        let body = get("/setup/eureka_info?params=name,does_not_exist").await;
        let object = body.as_object().unwrap();
        assert_eq!(object.len(), 1);
        assert_eq!(body["name"], "Living Room");
    }

    #[test]
    fn cloud_device_id_uses_clean_udn_or_md5() {
        // 32 alnum after stripping dashes → used directly (uppercased).
        assert_eq!(
            cloud_device_id("12345678-1234-1234-1234-123456789abc"),
            "12345678123412341234123456789ABC"
        );
        // Otherwise MD5 hex uppercase (32 chars).
        let hashed = cloud_device_id("not-a-uuid");
        assert_eq!(hashed.len(), 32);
    }
}
