//! App-author SDK for vibecast.
//!
//! Implement [`AppProvider`] (a factory) and [`AppSession`] (an owned,
//! per-launch session) to add a Cast app. `launch` returns a shared session
//! that *owns* its state for the session's lifetime; the runtime holds it
//! behind an [`Arc`] so callbacks can run off the routing task.
//!
//! App crates depend ONLY on this crate — no transport, TLS, or bridge types
//! leak in. The Cast protocol types apps need ([`LoadRequest`], [`MediaInfo`],
//! [`PlayerState`], etc.) are re-exported here so an app's entire dependency on
//! vibecast is this one crate.
//!
//! # Writing an app
//!
//! 1. Implement [`AppProvider`] — declare the Cast app ids, display name, and a
//!    stable `app_key` (used for per-app config and data directories). Override
//!    [`AppProvider::configure`] to receive the `[apps.<app_key>]` config block
//!    from `config.toml`; the default accepts and ignores it.
//! 2. Implement [`AppSession::resolve_media`] — translate a Cast `LOAD` request
//!    (the `content_id` in [`LoadRequest`]) into a [`PlaybackMedia`] describing
//!    the playable streams and DRM info. Failures map to typed
//!    [`MediaResolveError`] codes (sent back to the sender as `LOAD_FAILED`).
//! 3. Optionally override the other [`AppSession`] callbacks:
//!    - [`AppSession::on_message`] for custom-namespace messages (declared via
//!      [`AppProvider::namespaces`]).
//!    - [`AppSession::resolve_license`] to transform DRM challenges/responses
//!      before they hit the license proxy (e.g. Prime Video's custom flow). The
//!      default forwards unchanged.
//!    - [`AppSession::on_playback_update`] to react to canonical playback state
//!      (e.g. broadcast progress on a custom namespace).
//!    - [`AppSession::on_sender_connected`] / [`AppSession::on_stop`] for
//!      lifecycle hooks.
//! 4. Register the provider in `crates/vibecast-cli/src/main.rs::apps`.
//!
//! `vibecast-apps-svtplay` is the reference app — model new apps on it.
//!
//! # Minimal skeleton
//!
//! ```no_run
//! use std::sync::Arc;
//! use async_trait::async_trait;
//! use vibecast_sdk::{
//!     AppContext, AppProvider, AppSession, LaunchCredentials, LaunchError,
//!     LoadRequest, MediaResolveError, PlaybackMedia,
//! };
//!
//! pub struct MyApp;
//!
//! #[async_trait]
//! impl AppProvider for MyApp {
//!     fn app_ids(&self) -> &'static [&'static str] { &["DEADBEEF"] }
//!     fn display_name(&self) -> &'static str { "My App" }
//!     fn app_key(&self) -> &'static str { "myapp" }
//!
//!     async fn launch(
//!         &self,
//!         _ctx: &AppContext,
//!         _credentials: LaunchCredentials,
//!     ) -> Result<Arc<dyn AppSession>, LaunchError> {
//!         Ok(Arc::new(MySession))
//!     }
//! }
//!
//! pub struct MySession;
//!
//! #[async_trait]
//! impl AppSession for MySession {
//!     async fn resolve_media(
//!         &self,
//!         _ctx: &AppContext,
//!         request: &LoadRequest,
//!     ) -> Result<PlaybackMedia, MediaResolveError> {
//!         // resolve request.media.content_id into streams + DRM, then:
//!         # unreachable!()
//!     }
//! }
//! ```
//!
//! Generate the full API docs with `cargo doc -p vibecast-sdk --open`.

#![forbid(unsafe_code)]

mod config;
mod context;
mod error;
mod license;
mod types;

pub use config::{AppConfig, AppConfigError};
pub use context::{AppContext, NoopSenderChannel, ReceiverContext, SenderChannel};
pub use error::{LaunchError, MediaResolveCode, MediaResolveError};
pub use license::{LicenseForwarder, LicenseRequest, LicenseResponse, LicenseRoute};
pub use types::{
    DrmInfo, DrmSystem, LaunchCredentials, PlaybackMedia, PlaybackState, PlaybackStream,
};

// Re-export the Cast protocol types apps need so they depend on this crate only.
pub use vibecast_messages::{
    IdleReason, LoadRequest, MediaImage, MediaInfo, MediaMetadata, PlayerState, StreamType,
};

// Re-export the HTTP header types used in the license proxy API so app crates
// depend only on this crate.
pub use http::{HeaderMap, HeaderName, HeaderValue};

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

/// Outcome of an app custom-namespace message handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageDisposition {
    /// The app consumed the message.
    Handled,
    /// The app did not handle the message.
    Unhandled,
}

/// A Cast app: a factory that launches owned [`AppSession`]s.
#[async_trait]
pub trait AppProvider: Send + Sync {
    /// Cast application ids this app handles.
    fn app_ids(&self) -> &'static [&'static str];

    /// Human-readable name shown in receiver status.
    fn display_name(&self) -> &'static str;

    /// Stable key used for config and per-app data directories.
    fn app_key(&self) -> &'static str;

    /// Icon URL advertised in receiver status.
    fn icon_url(&self) -> Option<&'static str> {
        None
    }

    /// Custom namespaces (besides media) this app handles.
    fn namespaces(&self) -> &'static [&'static str] {
        &[]
    }

    /// Configure this provider before registration.
    ///
    /// Hosts call this once with the app-specific config block. The default
    /// implementation accepts and ignores config so simple apps stay minimal.
    fn configure(&mut self, _config: &AppConfig) -> Result<(), AppConfigError> {
        Ok(())
    }

    /// Launch a session for one of [`app_ids`](Self::app_ids).
    ///
    /// The returned session is shared: the runtime keeps it behind an [`Arc`]
    /// so per-sender callbacks can run outside the routing task.
    async fn launch(
        &self,
        ctx: &AppContext,
        credentials: LaunchCredentials,
    ) -> Result<Arc<dyn AppSession>, LaunchError>;
}

/// An owned, running app session.
#[async_trait]
pub trait AppSession: Send + Sync {
    /// Translate a Cast `LOAD` request into canonical playback media.
    async fn resolve_media(
        &self,
        ctx: &AppContext,
        request: &LoadRequest,
    ) -> Result<PlaybackMedia, MediaResolveError>;

    /// Handle a custom-namespace message (namespaces other than media).
    async fn on_message(
        &self,
        _ctx: &AppContext,
        _namespace: &str,
        _data: &Value,
    ) -> MessageDisposition {
        MessageDisposition::Unhandled
    }

    /// Called when a sender connects to this app transport.
    async fn on_sender_connected(&self, _ctx: &AppContext, _sender_id: &str) {}

    /// Resolve a proxied DRM license request. The default forwards it unchanged;
    /// override to transform the challenge/response (e.g. Prime Video).
    async fn resolve_license(
        &self,
        _ctx: &AppContext,
        request: LicenseRequest,
        route: LicenseRoute,
        forward: &dyn LicenseForwarder,
    ) -> LicenseResponse {
        forward.forward(request, route).await
    }

    /// Called when canonical playback state changes.
    async fn on_playback_update(&self, _ctx: &AppContext, _state: PlaybackState) {}

    /// Called before the session is torn down.
    async fn on_stop(&self, _ctx: &AppContext) {}
}

/// Normalize an app stream type to Cast media semantics (`NONE` -> `BUFFERED`).
#[must_use]
pub fn normalize_stream_type(stream_type: StreamType) -> StreamType {
    match stream_type {
        StreamType::None => StreamType::Buffered,
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_none_is_buffered() {
        assert_eq!(
            normalize_stream_type(StreamType::None),
            StreamType::Buffered
        );
    }

    #[test]
    fn normalize_passes_through_buffered_and_live() {
        assert_eq!(
            normalize_stream_type(StreamType::Buffered),
            StreamType::Buffered
        );
        assert_eq!(normalize_stream_type(StreamType::Live), StreamType::Live);
    }
}
