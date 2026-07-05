//! Stable app-author SDK for vibecast.
//!
//! Implement [`AppProvider`] (a factory) and [`AppSession`] (an owned,
//! per-launch session) to add a Cast app. `launch` returns a boxed session
//! that *owns* its state for the session's lifetime — there is no shared
//! session map to look up, so the "missing session state" error class from the
//! Python design does not exist here.
//!
//! App crates depend ONLY on this crate.

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
    async fn launch(
        &self,
        ctx: &AppContext,
        credentials: LaunchCredentials,
    ) -> Result<Box<dyn AppSession>, LaunchError>;
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
