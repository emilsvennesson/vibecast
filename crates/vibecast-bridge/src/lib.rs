//! Player bridge: the HTTP/WebSocket seam between the coordinator and external
//! players (browser / Kodi).
//!
//! The bridge serves the embedded Shaka Player page, relays [`PlayerCommand`]s
//! to players over a WebSocket, forwards [`PlayerReport`]s from the primary
//! player back to the coordinator, and proxies DRM license and DASH/HLS
//! manifest requests (with normalization) on behalf of the active session.
//!
//! The wire protocol, player/proxy seams, and manifest/header helpers live in
//! [`vibecast_player_api`]; settings schemas and player-scoped persistence live
//! in [`vibecast_settings`]. This crate connects both to one concrete
//! [`vibecast_player_api::Player`] plus [`vibecast_player_api::ProxyRegistrar`]
//! implementation.
//!
//! [`PlayerCommand`]: vibecast_player_api::PlayerCommand
//! [`PlayerReport`]: vibecast_player_api::PlayerReport

#![forbid(unsafe_code)]

mod bridge;
mod web;

pub use bridge::{PlayerBridge, PlayerEvent};
pub use vibecast_player_api::PlayerRegistration;
pub use web::{PLAYER_HTML, PLAYER_JS};
