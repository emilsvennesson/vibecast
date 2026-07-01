//! Small helpers for working with raw Cast JSON payloads.

use serde_json::Value;

/// Extract a request id from a raw payload, accepting `requestId` or
/// `request_id`, defaulting to 0 (used when building error responses for
/// payloads that failed typed validation).
#[must_use]
pub fn extract_request_id(raw: &Value) -> i64 {
    raw.get("requestId")
        .or_else(|| raw.get("request_id"))
        .and_then(Value::as_i64)
        .unwrap_or(0)
}
