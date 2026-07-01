//! DRM license and manifest proxy contracts.
//!
//! The bridge exposes `POST /license/{session}` and
//! `GET /manifest/{session}/{route}` routes. Resolution is delegated to
//! session-scoped handlers registered by the coordinator (ports the
//! `LicenseHandler` / `ManifestHandler` protocols and their request/response
//! dataclasses).

use std::collections::HashMap;

use async_trait::async_trait;

/// Error raised by a proxy handler; surfaced to the caller as HTTP 500.
#[derive(Debug, thiserror::Error)]
#[error("proxy handler failed: {0}")]
pub struct ProxyError(pub String);

/// Result alias for proxy handler methods.
pub type ProxyResult<T> = Result<T, ProxyError>;

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
    pub route_id: Option<String>,
    /// Filtered request headers.
    pub headers: HashMap<String, String>,
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
    /// Route selector (the `{route}` path segment, extension stripped).
    pub route_id: String,
    /// HTTP method (`GET` or `HEAD`).
    pub method: String,
    /// Filtered request headers.
    pub headers: HashMap<String, String>,
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
    pub headers: HashMap<String, String>,
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
