//! Developer capture proxy for reverse-engineering Cast apps and behavior.
//!
//! Sits between a Cast sender (phone) and a genuine receiver, logging **every**
//! CastV2 message (both directions) to `cast.jsonl`, and — via an `adb`-driven
//! transparent redirect on a rooted Android device — logging the receiver's
//! decrypted HTTP/HTTPS egress to `http.jsonl`. Both streams share a session
//! directory and a monotonic sequence so they can be merged after the fact.
//!
//! This is a developer tool: it deliberately records sensitive data (auth
//! flows, license/manifest requests) to disk. That output is git-ignored and
//! never touches the `tracing` subsystem.

#![forbid(unsafe_code)]

mod adb;
mod ca;
mod cast_proxy;
mod decode;
mod error;
mod http_mitm;
mod recorder;
mod tls;

use std::net::UdpSocket;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{Map, Value};
use tokio::net::TcpListener;

use vibecast_discovery::{CastAdvertisement, MdnsResponder};
use vibecast_security::{CertResolver, CertificateStore};

pub use error::CaptureError;

use crate::adb::Adb;
use crate::ca::CaptureCa;
use crate::cast_proxy::CastProxy;
use crate::http_mitm::HttpMitm;
use crate::recorder::Recorder;

/// Inputs for a capture session (the CLI fills these from its args + config).
#[derive(Debug, Clone)]
pub struct CaptureConfig {
    /// Harvested certificate manifest (same format the receiver uses).
    pub certs_path: PathBuf,
    /// Genuine receiver address to relay to.
    pub upstream_host: String,
    /// Genuine receiver CastV2 port (typically 8009).
    pub upstream_port: u16,
    /// Local port the proxy listens on for senders (typically 8009).
    pub listen_port: u16,
    /// Output directory; the session is written to `<out_dir>/<name>/`.
    pub out_dir: PathBuf,
    /// Session name; defaults to a UTC timestamp when `None`.
    pub name: Option<String>,
    /// Optional `adb` device serial (for multi-device hosts).
    pub adb_serial: Option<String>,
    /// MITM CA certificate + private key PEM (e.g. the two blocks of
    /// `~/.mitmproxy/mitmproxy-ca.pem`). When `Some`, the HTTP/HTTPS MITM is
    /// enabled; the CA must already be trusted on the device (Magisk cert
    /// module). When `None`, only Cast traffic is captured.
    pub ca: Option<CaMaterial>,
    /// Stable mDNS device id (kept across runs for consistent discovery).
    pub device_id: String,
    /// mDNS friendly name shown in sender apps.
    pub friendly_name: String,
    /// mDNS device model.
    pub model: String,
}

/// A MITM CA's certificate + private-key PEM.
#[derive(Debug, Clone)]
pub struct CaMaterial {
    /// CA certificate in PEM.
    pub cert_pem: String,
    /// CA private key in PEM (PKCS#8 or PKCS#1 RSA).
    pub key_pem: String,
}

/// Run a capture session until Ctrl-C, then tear everything down.
pub async fn run_capture(config: CaptureConfig) -> Result<(), CaptureError> {
    install_crypto_provider();

    // --- Certificates ----------------------------------------------------
    let store = CertificateStore::from_manifest_path(&config.certs_path)?;
    let bundle = store.active_bundle().clone();
    let crl = bundle.crl.clone();

    // --- Session directory + recorder ------------------------------------
    let name = config.name.clone().unwrap_or_else(recorder::timestamp_slug);
    let session_dir = config.out_dir.join(&name);
    std::fs::create_dir_all(&session_dir)?;
    let recorder = Arc::new(Recorder::create(&session_dir)?);
    tracing::info!(dir = %session_dir.display(), "capture session started");

    recorder.meta(
        "capture_start",
        obj([
            (
                "upstream",
                format!("{}:{}", config.upstream_host, config.upstream_port).into(),
            ),
            ("listen_port", config.listen_port.into()),
            ("capture_http", config.ca.is_some().into()),
            ("cert_digest", bundle.cert_digest_md5().into()),
        ]),
    );

    // --- Cast MITM -------------------------------------------------------
    let resolver = CertResolver::new(&bundle)?;
    let server_config =
        vibecast_security::server_config(resolver).map_err(|e| CaptureError::Tls(e.to_string()))?;
    let cast_listener = TcpListener::bind(("0.0.0.0", config.listen_port)).await?;

    let cast_proxy = Arc::new(CastProxy::new(
        Arc::clone(&recorder),
        server_config,
        bundle.clone(),
        crl,
        config.upstream_host.clone(),
        config.upstream_port,
    ));

    // --- mDNS advertisement ---------------------------------------------
    let advertisement = CastAdvertisement::new(
        &config.friendly_name,
        &config.model,
        &config.device_id,
        config.listen_port,
        &bundle.cert_digest_md5(),
    );
    let _responder = MdnsResponder::start(&advertisement)?;
    tracing::info!(
        name = %config.friendly_name,
        service = %advertisement.fullname(),
        "advertising capture proxy over mDNS"
    );

    // --- HTTP/HTTPS MITM (optional) --------------------------------------
    let mut adb = Adb::new(config.adb_serial.clone());
    let mut http_meta = Map::new();
    if let Some(ca) = &config.ca {
        match setup_http(&config, ca, &recorder, &mut adb).await {
            Ok(meta) => http_meta = meta,
            Err(error) => {
                tracing::error!(%error, "HTTP capture setup failed; continuing cast-only");
                recorder.meta(
                    "http_setup_failed",
                    obj([("error", error.to_string().into())]),
                );
            }
        }
    }

    // --- Write session metadata ------------------------------------------
    write_meta(
        &session_dir,
        &config,
        &name,
        &bundle.cert_digest_md5(),
        &http_meta,
    )?;

    // --- Serve until Ctrl-C ----------------------------------------------
    let cast_task = tokio::spawn(Arc::clone(&cast_proxy).serve(cast_listener));

    tracing::info!(
        "capture running — cast to \"{}\" from your sender. Press Ctrl-C to stop.",
        config.friendly_name
    );
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutting down capture");

    cast_task.abort();
    adb.teardown();
    recorder.meta("capture_end", Map::new());

    println!("Capture saved to {}", session_dir.display());
    Ok(())
}

/// Bind the MITM listeners, wire up the device redirect, return metadata.
async fn setup_http(
    config: &CaptureConfig,
    ca: &CaMaterial,
    recorder: &Arc<Recorder>,
    adb: &mut Adb,
) -> Result<Map<String, Value>, CaptureError> {
    adb.check_root()?;

    let ca = Arc::new(CaptureCa::from_pem(&ca.cert_pem, &ca.key_pem)?);

    let https_listener = TcpListener::bind(("0.0.0.0", 0)).await?;
    let http_listener = TcpListener::bind(("0.0.0.0", 0)).await?;
    let https_port = https_listener.local_addr()?.port();
    let http_port = http_listener.local_addr()?.port();

    let mitm = Arc::new(HttpMitm::new(Arc::clone(recorder), Arc::clone(&ca))?);
    tokio::spawn(Arc::clone(&mitm).serve_https(https_listener));
    tokio::spawn(mitm.serve_http(http_listener));

    let mac_ip = local_ip_towards(&config.upstream_host)
        .ok_or_else(|| CaptureError::Adb("could not determine local LAN IP".into()))?;

    adb.apply_redirect(&mac_ip, https_port, http_port)?;

    tracing::info!(%mac_ip, https_port, http_port, "device egress redirected to MITM");
    Ok(obj([
        ("mac_ip", mac_ip.into()),
        ("https_port", https_port.into()),
        ("http_port", http_port.into()),
    ]))
}

fn write_meta(
    dir: &std::path::Path,
    config: &CaptureConfig,
    name: &str,
    cert_digest: &str,
    http_meta: &Map<String, Value>,
) -> Result<(), CaptureError> {
    let meta = Value::Object(obj([
        ("name", name.into()),
        ("started", recorder::now_rfc3339().into()),
        (
            "upstream",
            format!("{}:{}", config.upstream_host, config.upstream_port).into(),
        ),
        ("listen_port", config.listen_port.into()),
        ("friendly_name", config.friendly_name.clone().into()),
        ("model", config.model.clone().into()),
        ("device_id", config.device_id.clone().into()),
        ("cert_digest", cert_digest.into()),
        ("capture_http", config.ca.is_some().into()),
        ("http", Value::Object(http_meta.clone())),
    ]));
    std::fs::write(dir.join("meta.json"), serde_json::to_vec_pretty(&meta)?)?;
    Ok(())
}

/// Determine which local IP the host would use to reach `target` (no traffic
/// is sent; the UDP socket just resolves the routing decision).
fn local_ip_towards(target: &str) -> Option<String> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect((target, 9)).ok()?;
    Some(socket.local_addr().ok()?.ip().to_string())
}

fn install_crypto_provider() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}

fn obj<const N: usize>(pairs: [(&str, Value); N]) -> Map<String, Value> {
    pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()
}
