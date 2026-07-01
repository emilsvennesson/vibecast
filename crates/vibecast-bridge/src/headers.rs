//! HTTP header filtering for the playback proxy flows.
//!
//! Ports `vibecast._playback.headers`: hop-by-hop headers must not be
//! forwarded when relaying license/manifest requests and responses.

use std::collections::HashMap;

/// Hop-by-hop request headers that must be stripped before forwarding upstream.
pub const HOP_BY_HOP_REQUEST_HEADERS: &[&str] = &[
    "connection",
    "content-length",
    "host",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// Hop-by-hop response headers that must be stripped before returning downstream.
pub const HOP_BY_HOP_RESPONSE_HEADERS: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// Response headers dropped by the manifest proxy (hop-by-hop plus entity
/// headers we always regenerate: encoding, length, type, and cookies).
pub const MANIFEST_PROXY_BLOCKED_RESPONSE_HEADERS: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "content-encoding",
    "content-length",
    "content-type",
    "set-cookie",
];

/// Drop hop-by-hop request headers before forwarding upstream.
#[must_use]
pub fn filter_upstream_headers(headers: &HashMap<String, String>) -> HashMap<String, String> {
    filter(headers, HOP_BY_HOP_REQUEST_HEADERS)
}

/// Drop blocked and hop-by-hop response headers.
#[must_use]
pub fn filter_upstream_response_headers(
    headers: &HashMap<String, String>,
) -> HashMap<String, String> {
    filter(headers, MANIFEST_PROXY_BLOCKED_RESPONSE_HEADERS)
}

/// Extract the token list named by any `Connection` header.
#[must_use]
pub fn connection_header_tokens(headers: &HashMap<String, String>) -> Vec<String> {
    let mut tokens = Vec::new();
    for (key, value) in headers {
        if !key.eq_ignore_ascii_case("connection") {
            continue;
        }
        for token in value.split(',') {
            let normalized = token.trim().to_ascii_lowercase();
            if !normalized.is_empty() {
                tokens.push(normalized);
            }
        }
    }
    tokens
}

fn filter(headers: &HashMap<String, String>, blocked: &[&str]) -> HashMap<String, String> {
    let connection_tokens = connection_header_tokens(headers);
    headers
        .iter()
        .filter(|(key, _)| {
            let lowered = key.to_ascii_lowercase();
            !blocked.contains(&lowered.as_str()) && !connection_tokens.contains(&lowered)
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn drops_hop_by_hop_request_headers() {
        let headers = map(&[
            ("Host", "example.com"),
            ("Content-Length", "10"),
            ("Authorization", "Bearer x"),
            ("X-Custom", "keep"),
        ]);
        let filtered = filter_upstream_headers(&headers);
        assert!(!filtered.contains_key("Host"));
        assert!(!filtered.contains_key("Content-Length"));
        assert_eq!(
            filtered.get("Authorization").map(String::as_str),
            Some("Bearer x")
        );
        assert_eq!(filtered.get("X-Custom").map(String::as_str), Some("keep"));
    }

    #[test]
    fn connection_named_tokens_are_dropped() {
        let headers = map(&[
            ("Connection", "keep-alive, X-Remove-Me"),
            ("X-Remove-Me", "1"),
            ("X-Preserved", "ok"),
        ]);
        let filtered = filter_upstream_response_headers(&headers);
        assert!(!filtered.contains_key("Connection"));
        assert!(!filtered.contains_key("X-Remove-Me"));
        assert_eq!(filtered.get("X-Preserved").map(String::as_str), Some("ok"));
    }

    #[test]
    fn manifest_proxy_strips_entity_and_hop_by_hop_headers() {
        let headers = map(&[
            ("Content-Encoding", "gzip"),
            ("Content-Length", "999"),
            ("Content-Type", "text/plain"),
            ("Set-Cookie", "sid=123"),
            ("Transfer-Encoding", "chunked"),
            ("X-Preserved", "ok"),
        ]);
        let filtered = filter_upstream_response_headers(&headers);
        for blocked in [
            "Content-Encoding",
            "Content-Length",
            "Content-Type",
            "Set-Cookie",
            "Transfer-Encoding",
        ] {
            assert!(
                !filtered.contains_key(blocked),
                "{blocked} should be dropped"
            );
        }
        assert_eq!(filtered.get("X-Preserved").map(String::as_str), Some("ok"));
    }
}
