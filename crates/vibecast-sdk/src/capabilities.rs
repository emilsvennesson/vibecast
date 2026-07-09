//! Player capabilities reported at registration and exposed to app sessions.
//!
//! Each Cast receiver is bound to exactly one player, so a player's
//! capabilities are part of its [`ReceiverContext`](crate::ReceiverContext).
//! Apps read these to make conditional decisions per selected player — e.g.
//! request only codecs the player can decode, cap the resolution at what the
//! player can output, or pick a DRM system the player supports at a sufficient
//! security level.

use crate::types::DrmSystem;

/// The runtime platform a player executes on.
///
/// Coarse hint some services use to shape their playback request (device type,
/// user-agent family). More precise decisions should use the typed capability
/// fields rather than branching on the platform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Platform {
    /// Android / Android TV.
    Android,
    /// Desktop Linux.
    Linux,
    /// macOS.
    MacOs,
    /// Windows.
    Windows,
    /// A web browser (e.g. the bundled Shaka page).
    Browser,
    /// Anything else, carrying the player's own label.
    Other(String),
}

impl Platform {
    /// The canonical lowercase token for this platform.
    #[must_use]
    pub fn as_token(&self) -> &str {
        match self {
            Platform::Android => "android",
            Platform::Linux => "linux",
            Platform::MacOs => "macos",
            Platform::Windows => "windows",
            Platform::Browser => "browser",
            Platform::Other(label) => label,
        }
    }
}

/// Widevine security tier a player can satisfy.
///
/// Services gate the maximum resolution they grant on this: L1 (hardware-backed)
/// typically unlocks HD/UHD, while L3 (software) is often limited to SD.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrmSecurityLevel {
    /// Hardware-backed, highest security (HD/UHD eligible).
    L1,
    /// Mixed hardware/software.
    L2,
    /// Software-only, lowest security (often SD-capped).
    L3,
}

/// A DRM system the player supports, with its security level where meaningful.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrmCapability {
    /// The DRM key system.
    pub system: DrmSystem,
    /// The security level the player can satisfy (Widevine); `None` when the
    /// concept does not apply (e.g. ClearKey) or is unknown.
    pub security_level: Option<DrmSecurityLevel>,
}

impl DrmCapability {
    /// A capability with a known security level.
    #[must_use]
    pub fn new(system: DrmSystem, security_level: Option<DrmSecurityLevel>) -> Self {
        Self {
            system,
            security_level,
        }
    }
}

/// A video output resolution (pixels).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resolution {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

impl Resolution {
    /// Build a resolution.
    #[must_use]
    pub const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }

    /// 1920x1080.
    #[must_use]
    pub const fn hd_1080() -> Self {
        Self::new(1920, 1080)
    }
}

/// Everything a player reports about what it can play.
///
/// Codec, HDR, and subtitle values use neutral lowercase tokens (e.g. `h264`,
/// `hevc`, `vp9`, `av1`; `hdr10`, `dolbyvision`, `hlg`; `ttml`, `vtt`); apps
/// translate these into their backend's own vocabulary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerCapabilities {
    /// The player's runtime platform.
    pub platform: Platform,
    /// DRM systems the player supports, with security levels.
    pub drm: Vec<DrmCapability>,
    /// Video codecs the player can decode (neutral tokens).
    pub video_codecs: Vec<String>,
    /// Audio codecs the player can decode (neutral tokens).
    pub audio_codecs: Vec<String>,
    /// Maximum video output resolution.
    pub max_resolution: Resolution,
    /// HDR formats the player can output; empty means SDR only.
    pub hdr_formats: Vec<String>,
    /// Frame rates (fps) the player can output.
    pub frame_rates: Vec<u32>,
    /// Subtitle/timed-text formats the player can render (neutral tokens).
    pub subtitle_formats: Vec<String>,
    /// Maximum HDCP level the output link can enforce (e.g. `"1.4"`, `"2.2"`),
    /// or `None` if unknown / not applicable.
    pub hdcp_level: Option<String>,
}

impl PlayerCapabilities {
    /// Whether the player supports a DRM system (at any level).
    #[must_use]
    pub fn supports_drm(&self, system: DrmSystem) -> bool {
        self.drm.iter().any(|cap| cap.system == system)
    }

    /// The security level the player satisfies for a DRM system, if supported.
    #[must_use]
    pub fn drm_level(&self, system: DrmSystem) -> Option<DrmSecurityLevel> {
        self.drm
            .iter()
            .find(|cap| cap.system == system)
            .and_then(|cap| cap.security_level)
    }

    /// Whether the player can decode the given (neutral-token) video codec.
    #[must_use]
    pub fn supports_video_codec(&self, codec: &str) -> bool {
        self.video_codecs.iter().any(|c| c == codec)
    }

    /// Whether the player can output any HDR format.
    #[must_use]
    pub fn supports_hdr(&self) -> bool {
        !self.hdr_formats.is_empty()
    }
}

impl Default for PlayerCapabilities {
    /// A conservative baseline: SDR 1080p, H.264/H.265 video, AAC audio, no DRM
    /// asserted. Used as a fallback until a player reports its real profile.
    fn default() -> Self {
        Self {
            platform: Platform::Other("unknown".to_string()),
            drm: Vec::new(),
            video_codecs: vec!["h264".to_string(), "hevc".to_string()],
            audio_codecs: vec!["aac".to_string()],
            max_resolution: Resolution::hd_1080(),
            hdr_formats: Vec::new(),
            frame_rates: vec![24, 25, 30, 50, 60],
            subtitle_formats: Vec::new(),
            hdcp_level: None,
        }
    }
}
