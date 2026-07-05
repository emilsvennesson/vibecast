//! HTTP header filtering for the playback proxy flows.
//!
//! Hop-by-hop headers must not be forwarded when relaying license/manifest
//! requests and responses. Operating on [`http::HeaderMap`] (rather than a
//! `HashMap<String, String>`) preserves duplicate headers, keeps values as
//! validated [`HeaderValue`]s, and reuses the canonical case-insensitive
//! [`HeaderName`] comparison instead of ad-hoc lowercasing.

use http::header::{HeaderMap, HeaderName};

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
pub fn filter_upstream_headers(headers: &HeaderMap) -> HeaderMap {
    filter(headers, HOP_BY_HOP_REQUEST_HEADERS)
}

/// Drop blocked and hop-by-hop response headers.
#[must_use]
pub fn filter_upstream_response_headers(headers: &HeaderMap) -> HeaderMap {
    filter(headers, MANIFEST_PROXY_BLOCKED_RESPONSE_HEADERS)
}

/// Extract the token list named by any `Connection` header.
#[must_use]
pub fn connection_header_tokens(headers: &HeaderMap) -> Vec<HeaderName> {
    let mut tokens = Vec::new();
    for value in headers.get_all(http::header::CONNECTION) {
        let Ok(value) = value.to_str() else { continue };
        for token in value.split(',') {
            if let Ok(name) = HeaderName::try_from(token.trim()) {
                tokens.push(name);
            }
        }
    }
    tokens
}

fn filter(headers: &HeaderMap, blocked: &[&str]) -> HeaderMap {
    let connection_tokens = connection_header_tokens(headers);
    let mut out = HeaderMap::with_capacity(headers.len());
    for (name, value) in headers {
        let name_str = name.as_str();
        let is_blocked = blocked.contains(&name_str) || connection_tokens.contains(name);
        if !is_blocked {
            out.append(name.clone(), value.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::header::HeaderValue;

    fn map(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.append(
                HeaderName::try_from(*name).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        headers
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
            filtered.get("Authorization").and_then(|v| v.to_str().ok()),
            Some("Bearer x")
        );
        assert_eq!(
            filtered.get("X-Custom").and_then(|v| v.to_str().ok()),
            Some("keep")
        );
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
        assert_eq!(
            filtered.get("X-Preserved").and_then(|v| v.to_str().ok()),
            Some("ok")
        );
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
        assert_eq!(
            filtered.get("X-Preserved").and_then(|v| v.to_str().ok()),
            Some("ok")
        );
    }
}
