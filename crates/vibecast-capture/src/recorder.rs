//! Shared JSON Lines recorder.
//!
//! Writes two append-only streams inside the session directory —
//! `cast.jsonl` (Cast protocol + lifecycle/meta events) and `http.jsonl`
//! (decrypted HTTP/HTTPS flows) — and stamps every record with a monotonic
//! `seq` plus a wall-clock timestamp so the two streams can be merged and
//! ordered after the fact.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};

use crate::error::CaptureError;

/// Two-stream JSONL writer shared across the cast + http tasks.
pub struct Recorder {
    cast: Mutex<BufWriter<File>>,
    http: Mutex<BufWriter<File>>,
    seq: AtomicU64,
}

impl Recorder {
    /// Open (create/truncate) the two log files under `dir`.
    pub fn create(dir: &Path) -> Result<Self, CaptureError> {
        Ok(Self {
            cast: Mutex::new(BufWriter::new(File::create(dir.join("cast.jsonl"))?)),
            http: Mutex::new(BufWriter::new(File::create(dir.join("http.jsonl"))?)),
            seq: AtomicU64::new(1),
        })
    }

    /// Append one object to `cast.jsonl` (Cast messages + meta events).
    pub fn cast(&self, entry: Map<String, Value>) {
        if entry.get("layer").and_then(Value::as_str) != Some("meta") {
            log_cast(&entry);
        }
        self.write(&self.cast, entry);
    }

    /// Append one object to `http.jsonl` (HTTP/HTTPS flows).
    pub fn http(&self, entry: Map<String, Value>) {
        log_http(&entry);
        self.write(&self.http, entry);
    }

    /// Convenience for lifecycle/meta events on the cast stream.
    pub fn meta(&self, event: &str, mut fields: Map<String, Value>) {
        fields.insert("layer".into(), Value::from("meta"));
        fields.insert("event".into(), Value::from(event));
        self.cast(fields);
    }

    fn write(&self, sink: &Mutex<BufWriter<File>>, mut entry: Map<String, Value>) {
        let (ts, ts_ms) = now();
        // Stamp ordering/time fields first so they lead each line.
        let mut stamped = Map::with_capacity(entry.len() + 3);
        stamped.insert(
            "seq".into(),
            Value::from(self.seq.fetch_add(1, Ordering::Relaxed)),
        );
        stamped.insert("ts".into(), Value::from(ts));
        stamped.insert("ts_unix_ms".into(), Value::from(ts_ms));
        stamped.append(&mut entry);

        let mut line = match serde_json::to_string(&Value::Object(stamped)) {
            Ok(line) => line,
            Err(error) => {
                tracing::warn!(%error, "failed to serialize capture record");
                return;
            }
        };
        line.push('\n');

        let mut guard = sink
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Err(error) = guard
            .write_all(line.as_bytes())
            .and_then(|()| guard.flush())
        {
            tracing::warn!(%error, "failed to write capture record");
        }
    }
}

/// Current wall-clock time as an RFC3339 UTC string.
pub(crate) fn now_rfc3339() -> String {
    now().0
}

/// Compact, filesystem-safe UTC timestamp (`YYYYMMDD_HHMMSS`) for default names.
pub(crate) fn timestamp_slug() -> String {
    // From "2026-07-20T12:40:00.789Z" -> "20260720_124000".
    let ts = now().0;
    let digits: String = ts.chars().take(19).filter(char::is_ascii_digit).collect();
    let (date, time) = digits.split_at(8.min(digits.len()));
    format!("{date}_{time}")
}

/// Current time as `(RFC3339 string, unix millis)`.
fn now() -> (String, u64) {
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    (
        rfc3339_utc(dur.as_secs(), dur.subsec_millis()),
        dur.as_millis() as u64,
    )
}

/// Format a Unix timestamp as an RFC3339 UTC string (`YYYY-MM-DDTHH:MM:SS.mmmZ`).
///
/// Uses Howard Hinnant's days-from-civil algorithm; no external date crate.
fn rfc3339_utc(unix_secs: u64, millis: u32) -> String {
    let days = (unix_secs / 86_400) as i64;
    let secs_of_day = unix_secs % 86_400;
    let (hour, minute, second) = (
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60,
    );

    // days-from-civil, inverted (epoch 1970-01-01 == day 0).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };

    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

fn log_cast(entry: &Map<String, Value>) {
    let dir = entry
        .get("direction")
        .and_then(Value::as_str)
        .unwrap_or("?");
    let ns = entry
        .get("namespace")
        .and_then(Value::as_str)
        .map(|n| n.strip_prefix("urn:x-cast:").unwrap_or(n))
        .unwrap_or("?");
    let ty = cast_type_label(entry.get("payload"));
    tracing::info!("cast  {dir}  {ns}  {ty}");
}

/// Extract a concise type label from a decoded payload value.
fn cast_type_label(payload: Option<&Value>) -> String {
    let Some(p) = payload else {
        return "(no payload)".into();
    };
    match p {
        Value::Null => "(empty)".into(),
        Value::String(s) => truncate(s, 40),
        Value::Object(obj) => {
            if let Some(t) = obj.get("type").and_then(Value::as_str) {
                return t.into();
            }
            if let Some(d) = obj.get("_decoded").and_then(Value::as_str) {
                return d.into();
            }
            if obj.contains_key("_binary_len") {
                return format!(
                    "binary({} bytes)",
                    obj.get("_binary_len").and_then(Value::as_u64).unwrap_or(0)
                );
            }
            let keys: Vec<&str> = obj.keys().map(String::as_str).collect();
            format!("{{{}}}", keys.join(","))
        }
        _ => truncate(&p.to_string(), 40),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.into()
    } else {
        format!("{}…", &s[..max])
    }
}

fn log_http(entry: &Map<String, Value>) {
    let method = entry.get("method").and_then(Value::as_str).unwrap_or("?");
    let host = entry.get("host").and_then(Value::as_str).unwrap_or("?");
    match entry.get("status").and_then(Value::as_u64) {
        Some(s) => tracing::info!("http  {method}  [{s}]  {host}"),
        None => tracing::info!("http  {method}  [--]  {host}"),
    }
}

#[cfg(test)]
mod tests {
    use super::rfc3339_utc;

    #[test]
    fn formats_known_epochs() {
        assert_eq!(rfc3339_utc(0, 0), "1970-01-01T00:00:00.000Z");
        assert_eq!(rfc3339_utc(946_684_800, 0), "2000-01-01T00:00:00.000Z");
        assert_eq!(rfc3339_utc(1_609_459_200, 0), "2021-01-01T00:00:00.000Z");
        assert_eq!(rfc3339_utc(1_784_551_200, 789), "2026-07-20T12:40:00.789Z");
    }
}
