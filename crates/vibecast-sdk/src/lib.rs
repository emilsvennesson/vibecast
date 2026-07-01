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

mod context;
mod error;
mod types;

pub use context::{AppContext, ReceiverContext};
pub use error::{LaunchError, MediaResolveCode, MediaResolveError};
pub use types::{
    DrmInfo, DrmSystem, LaunchCredentials, PlaybackMedia, PlaybackState, PlaybackStream,
};

// Re-export the Cast protocol types apps need so they depend on this crate only.
pub use vibecast_messages::{
    IdleReason, LoadRequest, MediaImage, MediaInfo, MediaMetadata, PlayerState, StreamType,
};

use async_trait::async_trait;

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
