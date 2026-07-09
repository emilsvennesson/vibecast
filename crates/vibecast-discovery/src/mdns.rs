//! Cast advertisement identity plus the (feature-gated) mDNS responder.
//!
//! [`CastAdvertisement`] is the **portable** half: it computes the service
//! instance label, SRV host target, and TXT record that identify the receiver
//! as a `_googlecast._tcp` device. It has no networking dependency and is
//! compiled on every platform — a frontend that owns discovery (Android
//! `NsdManager`, iOS `NWListener`) consumes its [`instance`](CastAdvertisement::instance)
//! and [`txt`](CastAdvertisement::txt) to register the service itself.
//!
//! [`MdnsResponder`] is the desktop half: it actually announces the service
//! over multicast DNS via `mdns-sd`. It lives behind the `mdns` cargo feature
//! so platforms that advertise through a native API never link `mdns-sd`.

use md5::{Digest, Md5};

#[cfg(feature = "mdns")]
use crate::error::DiscoveryError;

const SERVICE_TYPE: &str = "_googlecast._tcp.local.";
const INSTANCE_PREFIX: &str = "vibecast-";
const MAX_LABEL_LENGTH: usize = 63;

/// Structured Cast TXT record payload (short keys are the on-wire names).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CastServiceTxt {
    /// `ve` — TXT version.
    pub ve: String,
    /// `md` — device model.
    pub md: String,
    /// `fn` — friendly name.
    pub friendly_name: String,
    /// `id` — device id (dashes stripped).
    pub id: String,
    /// `cd` — certificate digest (uppercase hex).
    pub cd: String,
    /// `ca` — capabilities bitfield.
    pub ca: String,
    /// `bs` — hashed device id.
    pub bs: String,
    /// `st` — status.
    pub st: String,
    /// `nf` — notification flag.
    pub nf: String,
    /// `ic` — icon path.
    pub ic: String,
    /// `rs` — receiver status text.
    pub rs: String,
    /// `rm` — reserved.
    pub rm: String,
}

impl CastServiceTxt {
    fn new(model: &str, friendly_name: &str, clean_id: &str, cert_digest: &str, bs: &str) -> Self {
        Self {
            ve: "05".into(),
            md: model.into(),
            friendly_name: friendly_name.into(),
            id: clean_id.into(),
            cd: cert_digest.to_uppercase(),
            ca: "463365".into(),
            bs: bs.into(),
            st: "0".into(),
            nf: "1".into(),
            ic: "/setup/icon.png".into(),
            rs: String::new(),
            rm: String::new(),
        }
    }

    /// TXT key/value pairs in a stable order.
    #[must_use]
    pub fn pairs(&self) -> Vec<(&'static str, String)> {
        vec![
            ("ve", self.ve.clone()),
            ("md", self.md.clone()),
            ("fn", self.friendly_name.clone()),
            ("id", self.id.clone()),
            ("cd", self.cd.clone()),
            ("ca", self.ca.clone()),
            ("bs", self.bs.clone()),
            ("st", self.st.clone()),
            ("nf", self.nf.clone()),
            ("ic", self.ic.clone()),
            ("rs", self.rs.clone()),
            ("rm", self.rm.clone()),
        ]
    }

    #[cfg(feature = "mdns")]
    fn as_map(&self) -> std::collections::HashMap<String, String> {
        self.pairs()
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect()
    }
}

fn clean_device_id(device_id: &str) -> String {
    device_id.replace('-', "")
}

fn instance_name(clean_id: &str) -> String {
    let max_id_len = MAX_LABEL_LENGTH - INSTANCE_PREFIX.len();
    let truncated: String = clean_id.chars().take(max_id_len).collect();
    if truncated.is_empty() {
        "vibecast".into()
    } else {
        format!("{INSTANCE_PREFIX}{truncated}")
    }
}

fn compute_bs(device_id: &str) -> String {
    let digest = Md5::digest(device_id.as_bytes());
    hex::encode_upper(&digest[..6])
}

/// Canonical `<uuid>.local.` host target, or a sanitized fallback.
fn server_name(device_id: &str, clean_id: &str) -> String {
    if let Some(uuid) = format_uuid(clean_id) {
        return format!("{uuid}.local.");
    }
    let safe = device_id.trim().trim_matches('.');
    let safe = if safe.is_empty() { "vibecast" } else { safe };
    format!("{safe}.local.")
}

/// Format 32 hex chars as canonical lowercase UUID (8-4-4-4-12), else `None`.
fn format_uuid(clean_id: &str) -> Option<String> {
    if clean_id.len() != 32 || !clean_id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let lower = clean_id.to_ascii_lowercase();
    Some(format!(
        "{}-{}-{}-{}-{}",
        &lower[0..8],
        &lower[8..12],
        &lower[12..16],
        &lower[16..20],
        &lower[20..32],
    ))
}

/// Normalize app ids to sorted, deduplicated 8-char uppercase hex.
fn normalize_app_ids<I, S>(app_ids: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut normalized: Vec<String> = app_ids
        .into_iter()
        .filter_map(|raw| {
            let id = raw.as_ref().trim().to_ascii_uppercase();
            (id.len() == 8 && id.bytes().all(|b| b.is_ascii_hexdigit())).then_some(id)
        })
        .collect();
    normalized.sort();
    normalized.dedup();
    normalized
}

fn subtype_type(app_id: &str) -> String {
    format!("_{app_id}._sub.{SERVICE_TYPE}")
}

/// The portable identity of a Cast advertisement: the service instance label,
/// SRV host target, port, TXT record, and per-app subtypes.
///
/// This carries no networking state. On desktop it feeds `MdnsResponder`; on
/// platforms with native discovery it feeds the frontend's own registration
/// (via [`instance`](Self::instance) and [`txt`](Self::txt)).
pub struct CastAdvertisement {
    instance: String,
    server: String,
    port: u16,
    txt: CastServiceTxt,
    // Consumed only by the mDNS responder; a native-discovery frontend
    // registers the base service and ignores per-app subtypes.
    #[cfg_attr(not(feature = "mdns"), allow(dead_code))]
    subtype_types: Vec<String>,
}

impl CastAdvertisement {
    /// Compute the advertisement identity for a receiver.
    pub fn new<I, S>(
        friendly_name: &str,
        device_model: &str,
        device_id: &str,
        port: u16,
        cert_digest: &str,
        app_ids: I,
    ) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let clean_id = clean_device_id(device_id);
        let subtype_types = normalize_app_ids(app_ids)
            .iter()
            .map(|id| subtype_type(id))
            .collect();
        Self {
            instance: instance_name(&clean_id),
            server: server_name(device_id, &clean_id),
            port,
            txt: CastServiceTxt::new(
                device_model,
                friendly_name,
                &clean_id,
                cert_digest,
                &compute_bs(device_id),
            ),
            subtype_types,
        }
    }

    /// The advertised TXT record.
    #[must_use]
    pub fn txt(&self) -> &CastServiceTxt {
        &self.txt
    }

    /// The bare service instance label (e.g. `vibecast-<id>`), without the
    /// service type — what a foreign discovery registrar (e.g. Android's
    /// `NsdManager`) uses as the service name.
    #[must_use]
    pub fn instance(&self) -> &str {
        &self.instance
    }

    /// Fully-qualified base service instance name.
    #[must_use]
    pub fn fullname(&self) -> String {
        format!("{}.{SERVICE_TYPE}", self.instance)
    }

    /// The SRV host target advertised.
    #[must_use]
    pub fn server(&self) -> &str {
        &self.server
    }

    /// The advertised port.
    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Update the advertised certificate digest (on cert rotation). Returns
    /// `true` if the digest changed. Callers driving a live `MdnsResponder`
    /// should re-announce via `MdnsResponder::refresh` when this is `true`.
    pub fn update_cert_digest(&mut self, cert_digest: &str) -> bool {
        let digest = cert_digest.to_uppercase();
        if self.txt.cd == digest {
            return false;
        }
        self.txt.cd = digest;
        true
    }
}

/// Build the mDNS `ServiceInfo` for a given service/subtype domain.
#[cfg(feature = "mdns")]
fn service_info(
    advertisement: &CastAdvertisement,
    ty_domain: &str,
) -> Result<mdns_sd::ServiceInfo, DiscoveryError> {
    let info = mdns_sd::ServiceInfo::new(
        ty_domain,
        &advertisement.instance,
        &advertisement.server,
        "",
        advertisement.port,
        advertisement.txt.as_map(),
    )
    .map_err(|e| DiscoveryError::Mdns(e.to_string()))?
    .enable_addr_auto();
    Ok(info)
}

/// A live mDNS responder announcing a [`CastAdvertisement`] over multicast DNS.
///
/// Only available with the `mdns` cargo feature. Dropping it stops advertising.
#[cfg(feature = "mdns")]
pub struct MdnsResponder {
    daemons: Vec<mdns_sd::ServiceDaemon>,
}

#[cfg(feature = "mdns")]
impl MdnsResponder {
    /// Start advertising `advertisement`. The base service is required; per-app
    /// subtypes are best-effort — each needs its own responder because they
    /// share one instance name, which `mdns-sd` cannot register from a single
    /// daemon.
    pub fn start(advertisement: &CastAdvertisement) -> Result<Self, DiscoveryError> {
        use mdns_sd::ServiceDaemon;

        let base = ServiceDaemon::new().map_err(|e| DiscoveryError::Mdns(e.to_string()))?;
        base.register(service_info(advertisement, SERVICE_TYPE)?)
            .map_err(|e| DiscoveryError::Mdns(e.to_string()))?;
        let mut daemons = vec![base];

        for subtype in &advertisement.subtype_types {
            let daemon = match ServiceDaemon::new() {
                Ok(daemon) => daemon,
                Err(err) => {
                    tracing::warn!(%subtype, error = %err, "failed to create mDNS subtype responder");
                    continue;
                }
            };
            let info = match service_info(advertisement, subtype) {
                Ok(info) => info,
                Err(err) => {
                    tracing::warn!(%subtype, error = %err, "failed to build mDNS subtype service");
                    continue;
                }
            };
            if let Err(err) = daemon.register(info) {
                tracing::warn!(%subtype, %err, "failed to register mDNS subtype");
            } else {
                daemons.push(daemon);
            }
        }

        tracing::info!(
            service = %advertisement.fullname(),
            subtypes = advertisement.subtype_types.len(),
            "mDNS advertisement started"
        );
        Ok(Self { daemons })
    }

    /// Stop advertising and shut down all responders.
    pub fn stop(&mut self) {
        for daemon in self.daemons.drain(..) {
            let _ = daemon.shutdown();
        }
    }

    /// Re-announce after the advertisement's TXT changed (e.g. cert rotation).
    ///
    /// Re-registration goes through a full stop/start so the new TXT is
    /// re-announced regardless of responder caching. Certificate rotation is
    /// rare, so the brief re-advertise is acceptable.
    pub fn refresh(&mut self, advertisement: &CastAdvertisement) -> Result<(), DiscoveryError> {
        self.stop();
        *self = Self::start(advertisement)?;
        Ok(())
    }
}

#[cfg(feature = "mdns")]
impl Drop for MdnsResponder {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleans_and_builds_instance_and_bs() {
        assert_eq!(clean_device_id("ab-cd-ef"), "abcdef");
        assert_eq!(instance_name("deadbeef"), "vibecast-deadbeef");
        assert_eq!(instance_name(""), "vibecast");
        // bs = uppercase hex of first 6 md5 bytes of the raw device id.
        assert_eq!(compute_bs("device-1").len(), 12);
        assert!(compute_bs("device-1")
            .bytes()
            .all(|b| b.is_ascii_hexdigit()));
    }

    #[test]
    fn update_cert_digest_reports_change_and_uppercases() {
        let mut advertisement = CastAdvertisement::new(
            "Living Room",
            "Chromecast",
            "dev-id",
            8009,
            "abc123",
            Vec::<String>::new(),
        );
        assert_eq!(advertisement.txt().cd, "ABC123");
        // A no-op update (same digest, different case) reports no change.
        assert!(!advertisement.update_cert_digest("abc123"));
        // A real change reports `true` and stores the uppercased digest.
        assert!(advertisement.update_cert_digest("def456"));
        assert_eq!(advertisement.txt().cd, "DEF456");
    }

    #[test]
    fn server_name_prefers_canonical_uuid() {
        let clean = "12345678123412341234123456789abc";
        assert_eq!(
            server_name("12345678-1234-1234-1234-123456789abc", clean),
            "12345678-1234-1234-1234-123456789abc.local."
        );
        // Non-UUID id falls back to sanitized form.
        assert_eq!(
            server_name("living-room", "livingroom"),
            "living-room.local."
        );
        assert_eq!(server_name("", ""), "vibecast.local.");
    }

    #[test]
    fn normalizes_and_sorts_app_ids() {
        let ids = normalize_app_ids(["95370a1c", "17608BC8", "bad", "17608bc8", "ZZZZZZZZ"]);
        assert_eq!(ids, vec!["17608BC8".to_string(), "95370A1C".to_string()]);
    }

    #[test]
    fn subtype_type_format() {
        assert_eq!(
            subtype_type("95370A1C"),
            "_95370A1C._sub._googlecast._tcp.local."
        );
    }

    #[test]
    fn txt_record_matches_cast_layout() {
        let ad = CastAdvertisement::new(
            "Living Room",
            "Chromecast",
            "12345678-1234-1234-1234-123456789abc",
            8009,
            "abcdef", // lower-case digest, expect uppercased
            ["95370A1C"],
        );
        let txt = ad.txt();
        assert_eq!(txt.ve, "05");
        assert_eq!(txt.md, "Chromecast");
        assert_eq!(txt.friendly_name, "Living Room");
        assert_eq!(txt.id, "12345678123412341234123456789abc");
        assert_eq!(txt.cd, "ABCDEF"); // uppercased
        assert_eq!(txt.ca, "463365");
        assert_eq!(txt.st, "0");
        assert_eq!(txt.ic, "/setup/icon.png");
        // "fn" key is present in the on-wire pairs.
        assert!(txt
            .pairs()
            .iter()
            .any(|(k, v)| *k == "fn" && v == "Living Room"));
    }

    #[cfg(feature = "mdns")]
    #[test]
    fn base_and_subtype_service_info_construct_correctly() {
        let ad = CastAdvertisement::new(
            "Living Room",
            "Chromecast",
            "12345678-1234-1234-1234-123456789abc",
            8009,
            "ABCDEF",
            ["95370A1C"],
        );

        let base = service_info(&ad, SERVICE_TYPE).unwrap();
        assert_eq!(base.get_type(), SERVICE_TYPE);
        assert_eq!(base.get_port(), 8009);
        assert!(base.get_fullname().starts_with("vibecast-"));
        assert_eq!(base.get_property_val_str("md").unwrap(), "Chromecast");

        // Subtype spike: mdns-sd must parse and expose the subtype.
        let subtype = service_info(&ad, "_95370A1C._sub._googlecast._tcp.local.").unwrap();
        assert_eq!(
            subtype.get_subtype().as_deref(),
            Some("_95370A1C._sub._googlecast._tcp.local.")
        );
    }
}
