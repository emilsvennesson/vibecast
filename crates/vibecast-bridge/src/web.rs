//! Embedded web-player assets.
//!
//! Only two files ship with the receiver, so they are compiled into the binary
//! with `include_str!` rather than read from disk at runtime.

/// The browser player HTML page (Shaka Player host).
pub const PLAYER_HTML: &str = include_str!("../assets/player.html");

/// The browser player JavaScript (Shaka Player + WebSocket client).
pub const PLAYER_JS: &str = include_str!("../assets/player.js");

/// Content type served for [`PLAYER_HTML`].
pub(crate) const PLAYER_HTML_CONTENT_TYPE: &str = "text/html; charset=utf-8";

/// Content type served for [`PLAYER_JS`].
pub(crate) const PLAYER_JS_CONTENT_TYPE: &str = "application/javascript; charset=utf-8";
