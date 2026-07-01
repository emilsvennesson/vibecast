//! Embedded web-player assets.
//!
//! Only two files ship with the receiver, so they are compiled in with
//! `include_str!` rather than pulled from disk at runtime (mirrors the
//! Python `importlib.resources` loader, minus the filesystem).

/// The browser renderer HTML page (Shaka Player host).
pub const PLAYER_HTML: &str = include_str!("../assets/player.html");

/// The browser renderer JavaScript (Shaka Player + WebSocket client).
pub const PLAYER_JS: &str = include_str!("../assets/player.js");

/// Content type served for [`PLAYER_HTML`].
pub const PLAYER_HTML_CONTENT_TYPE: &str = "text/html; charset=utf-8";

/// Content type served for [`PLAYER_JS`].
pub const PLAYER_JS_CONTENT_TYPE: &str = "application/javascript; charset=utf-8";
