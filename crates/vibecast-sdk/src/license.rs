//! DRM license proxy types for the app-overridable `resolve_license` hook.
//!
//! By default the coordinator forwards a license request unchanged to the
//! stream's license URL (via a [`LicenseForwarder`] it supplies). Apps that
//! need custom handling (e.g. Prime Video's Amazon Widevine flow) override
//! [`AppSession::resolve_license`](crate::AppSession::resolve_license) and do
//! their own HTTP, optionally still calling `forward`.

use std::collections::HashMap;

use async_trait::async_trait;

use crate::DrmSystem;

/// A DRM license request forwarded from the renderer.
#[derive(Debug, Clone)]
pub struct LicenseRequest {
    /// Owning session id.
    pub session_id: String,
    /// Raw license challenge body.
    pub body: Vec<u8>,
    /// Request content type.
    pub content_type: String,
    /// Route selector identifying which stream's DRM applies.
    pub route_id: Option<String>,
    /// Filtered request headers.
    pub headers: HashMap<String, String>,
}

/// The license response returned to the renderer.
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
            content_type: "application/octet-stream".to_string(),
            status: 200,
        }
    }
}

/// The resolved upstream license target for one stream.
#[derive(Debug, Clone)]
pub struct LicenseRoute {
    /// Route selector (`r<index>`).
    pub route_id: String,
    /// Key system.
    pub system: DrmSystem,
    /// Upstream license acquisition URL.
    pub upstream_url: String,
    /// Extra headers to attach when forwarding.
    pub headers: HashMap<String, String>,
}

/// Forwards a license request to its upstream URL (the coordinator's default
/// behavior), given to `resolve_license` so apps can delegate to it.
#[async_trait]
pub trait LicenseForwarder: Send + Sync {
    /// Forward the request to `route.upstream_url` and return the response.
    async fn forward(&self, request: LicenseRequest, route: LicenseRoute) -> LicenseResponse;
}
