//! DRM license and manifest proxy contracts.
//!
//! The bridge exposes `POST /license/{session}` and
//! `GET /manifest/{session}/{route}` routes. Resolution is delegated to
//! session-scoped handlers registered by the coordinator. Requests and
//! responses carry validated [`HeaderMap`]s and typed [`RouteId`] selectors so
//! the proxy path never round-trips headers or route ids through lossy string
//! maps.

use std::fmt;
use std::str::FromStr;

use async_trait::async_trait;
use http::HeaderMap;

/// Error raised by a proxy handler; surfaced to the caller as HTTP 500.
#[derive(Debug, thiserror::Error)]
pub enum ProxyError {
    /// An upstream fetch (manifest/license origin) failed.
    #[error("upstream request failed: {0}")]
    Upstream(String),
    /// The handler could not fulfil the request for an internal reason.
    #[error("proxy handler failed: {0}")]
    Internal(String),
}

/// Result alias for proxy handler methods.
pub type ProxyResult<T> = Result<T, ProxyError>;

/// Which proxied resource a [`RouteId`] addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RouteKind {
    /// A DASH/HLS manifest route (serialized as `m{index}`).
    Manifest,
    /// A DRM license route (serialized as `r{index}`).
    License,
}

/// A parsed proxy-route selector: a resource kind plus the stream index it
/// applies to. Built once when generating proxy URLs and parsed once at the
/// HTTP boundary, so route dispatch is never stringly typed internally.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RouteId {
    kind: RouteKind,
    index: usize,
}

impl RouteId {
    /// A manifest route for the stream at `index`.
    #[must_use]
    pub const fn manifest(index: usize) -> Self {
        Self {
            kind: RouteKind::Manifest,
            index,
        }
    }

    /// A license route for the stream at `index`.
    #[must_use]
    pub const fn license(index: usize) -> Self {
        Self {
            kind: RouteKind::License,
            index,
        }
    }

    /// The resource kind this route addresses.
    #[must_use]
    pub const fn kind(self) -> RouteKind {
        self.kind
    }

    /// The stream index this route applies to.
    #[must_use]
    pub const fn index(self) -> usize {
        self.index
    }
}

impl fmt::Display for RouteId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let prefix = match self.kind {
            RouteKind::Manifest => 'm',
            RouteKind::License => 'r',
        };
        write!(f, "{prefix}{}", self.index)
    }
}

/// The route selector could not be parsed from its wire string.
#[derive(Debug, thiserror::Error)]
#[error("invalid route id {0:?}")]
pub struct RouteIdParseError(String);

impl FromStr for RouteId {
    type Err = RouteIdParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut chars = s.chars();
        let kind = match chars.next() {
            Some('m') => RouteKind::Manifest,
            Some('r') => RouteKind::License,
            _ => return Err(RouteIdParseError(s.to_string())),
        };
        let index: usize = chars
            .as_str()
            .parse()
            .map_err(|_| RouteIdParseError(s.to_string()))?;
        Ok(Self { kind, index })
    }
}

/// A DRM license request forwarded from the renderer.
#[derive(Debug, Clone)]
pub struct LicenseRequest {
    /// Owning session id.
    pub session_id: String,
    /// Raw license challenge body.
    pub body: Vec<u8>,
    /// Request content type.
    pub content_type: String,
    /// Route selector (`?route=...`), identifying which stream's DRM applies.
    pub route_id: Option<RouteId>,
    /// Filtered request headers.
    pub headers: HeaderMap,
}

/// A DRM license response returned to the renderer.
#[derive(Debug, Clone)]
pub struct LicenseResponse {
    /// Raw license body.
    pub body: Vec<u8>,
    /// Response content type.
    pub content_type: String,
    /// HTTP status.
    pub status: u16,
}

impl LicenseResponse {
    /// A 200 response with the default `application/octet-stream` type.
    #[must_use]
    pub fn ok(body: Vec<u8>) -> Self {
        Self {
            body,
            content_type: "application/octet-stream".into(),
            status: 200,
        }
    }
}

/// A manifest request received by the proxy route.
#[derive(Debug, Clone)]
pub struct ManifestProxyRequest {
    /// Owning session id.
    pub session_id: String,
    /// Route selector (parsed from the `{route}` path segment).
    pub route_id: RouteId,
    /// HTTP method (`GET` or `HEAD`).
    pub method: http::Method,
    /// Filtered request headers.
    pub headers: HeaderMap,
}

/// A manifest response returned by the handler (before the bridge filters
/// hop-by-hop response headers and sets the content type).
#[derive(Debug, Clone)]
pub struct ManifestProxyResponse {
    /// Response body (already normalized).
    pub body: Vec<u8>,
    /// Content type to serve.
    pub content_type: String,
    /// HTTP status.
    pub status: u16,
    /// Upstream response headers to forward (filtered by the bridge).
    pub headers: HeaderMap,
}

/// Session-scoped DRM license resolver.
#[async_trait]
pub trait LicenseHandler: Send + Sync {
    /// Resolve one proxied license request.
    async fn handle_license(&self, request: LicenseRequest) -> ProxyResult<LicenseResponse>;
}

/// Session-scoped manifest resolver.
#[async_trait]
pub trait ManifestHandler: Send + Sync {
    /// Resolve one proxied manifest request.
    async fn handle_manifest(
        &self,
        request: ManifestProxyRequest,
    ) -> ProxyResult<ManifestProxyResponse>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_id_roundtrips_through_its_wire_string() {
        assert_eq!("m0".parse::<RouteId>().unwrap(), RouteId::manifest(0));
        assert_eq!("r7".parse::<RouteId>().unwrap(), RouteId::license(7));
        assert_eq!(RouteId::manifest(3).to_string(), "m3");
        assert_eq!(RouteId::license(12).to_string(), "r12");
    }

    #[test]
    fn route_id_rejects_malformed_selectors() {
        assert!("x1".parse::<RouteId>().is_err());
        assert!("m".parse::<RouteId>().is_err());
        assert!("mabc".parse::<RouteId>().is_err());
        assert!("".parse::<RouteId>().is_err());
    }
}
