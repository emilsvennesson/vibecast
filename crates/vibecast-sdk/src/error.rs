//! Error types for the app SDK.

/// Canonical app media-resolution failure reasons. The string form is used as
/// the `LOAD_FAILED` reason sent to senders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaResolveCode {
    /// Malformed or unsupported request.
    InvalidRequest,
    /// Authentication is required.
    AuthRequired,
    /// Access was denied.
    AccessDenied,
    /// Required context (e.g. credentials) was missing.
    MissingContext,
    /// Requested content is unavailable.
    ContentUnavailable,
    /// An upstream dependency failed.
    UpstreamFailure,
    /// The player failed to start.
    PlayerFailure,
    /// An unexpected internal error.
    InternalError,
}

impl MediaResolveCode {
    /// The canonical string form used as the `LOAD_FAILED` reason.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            MediaResolveCode::InvalidRequest => "INVALID_REQUEST",
            MediaResolveCode::AuthRequired => "AUTH_REQUIRED",
            MediaResolveCode::AccessDenied => "ACCESS_DENIED",
            MediaResolveCode::MissingContext => "MISSING_CONTEXT",
            MediaResolveCode::ContentUnavailable => "CONTENT_UNAVAILABLE",
            MediaResolveCode::UpstreamFailure => "UPSTREAM_FAILURE",
            MediaResolveCode::PlayerFailure => "PLAYER_FAILURE",
            MediaResolveCode::InternalError => "INTERNAL_ERROR",
        }
    }

    fn default_retryable(self) -> bool {
        matches!(self, MediaResolveCode::UpstreamFailure)
    }
}

/// A structured media-resolution failure returned by an app session.
#[derive(Debug, Clone)]
pub struct MediaResolveError {
    /// Canonical failure code (drives the `LOAD_FAILED` reason).
    pub code: MediaResolveCode,
    /// App-specific detail code for diagnostics.
    pub detail_code: Option<String>,
    /// Human-readable message.
    pub message: Option<String>,
    /// Whether retrying the request may succeed.
    pub retryable: bool,
}

impl MediaResolveError {
    /// Build a failure with the code's default retryability.
    #[must_use]
    pub fn new(code: MediaResolveCode, detail_code: impl Into<String>) -> Self {
        Self {
            code,
            detail_code: Some(detail_code.into()),
            message: None,
            retryable: code.default_retryable(),
        }
    }

    /// Attach a human-readable message.
    #[must_use]
    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.message = Some(message.into());
        self
    }

    /// An `INVALID_REQUEST` failure.
    #[must_use]
    pub fn invalid_request(detail_code: impl Into<String>) -> Self {
        Self::new(MediaResolveCode::InvalidRequest, detail_code)
    }

    /// A `CONTENT_UNAVAILABLE` failure.
    #[must_use]
    pub fn content_unavailable(detail_code: impl Into<String>) -> Self {
        Self::new(MediaResolveCode::ContentUnavailable, detail_code)
    }

    /// An `INTERNAL_ERROR` failure.
    #[must_use]
    pub fn internal(detail_code: impl Into<String>) -> Self {
        Self::new(MediaResolveCode::InternalError, detail_code)
    }

    /// Map one upstream HTTP status code to a canonical failure.
    #[must_use]
    pub fn from_http_status(status: u16, detail_code: Option<String>) -> Self {
        let (code, retryable) = match status {
            401 => (MediaResolveCode::AuthRequired, false),
            403 => (MediaResolveCode::AccessDenied, false),
            404 => (MediaResolveCode::ContentUnavailable, false),
            429 => (MediaResolveCode::UpstreamFailure, true),
            s if s >= 500 => (MediaResolveCode::UpstreamFailure, true),
            s if s >= 400 => (MediaResolveCode::InvalidRequest, false),
            _ => (MediaResolveCode::UpstreamFailure, true),
        };
        Self {
            code,
            detail_code: Some(detail_code.unwrap_or_else(|| format!("UPSTREAM_{status}"))),
            message: None,
            retryable,
        }
    }

    /// The canonical `LOAD_FAILED` reason string.
    #[must_use]
    pub fn reason(&self) -> &'static str {
        self.code.as_str()
    }
}

impl std::fmt::Display for MediaResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.code.as_str())?;
        if let Some(detail) = &self.detail_code {
            write!(f, " ({detail})")?;
        }
        if let Some(message) = &self.message {
            write!(f, ": {message}")?;
        }
        Ok(())
    }
}

impl std::error::Error for MediaResolveError {}

impl From<reqwest::Error> for MediaResolveError {
    fn from(error: reqwest::Error) -> Self {
        if let Some(status) = error.status() {
            return Self::from_http_status(status.as_u16(), None).with_message(error.to_string());
        }
        let retryable = error.is_timeout() || error.is_connect();
        Self {
            code: MediaResolveCode::UpstreamFailure,
            detail_code: Some("HTTP_ERROR".to_string()),
            message: Some(error.to_string()),
            retryable,
        }
    }
}

/// Raised when an app fails to launch.
///
/// Carries a human-readable message and an optional underlying cause so hosts
/// can log the full error chain instead of a flattened string.
#[derive(Debug, thiserror::Error)]
#[error("app launch failed: {message}")]
pub struct LaunchError {
    message: String,
    #[source]
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl LaunchError {
    /// A launch failure with a message and no underlying cause.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            source: None,
        }
    }

    /// A launch failure wrapping an underlying error as its `#[source]`.
    #[must_use]
    pub fn with_source(
        message: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            message: message.into(),
            source: Some(Box::new(source)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_status_maps_to_canonical_codes() {
        let cases = [
            (401, MediaResolveCode::AuthRequired, false),
            (403, MediaResolveCode::AccessDenied, false),
            (404, MediaResolveCode::ContentUnavailable, false),
            (429, MediaResolveCode::UpstreamFailure, true),
            (503, MediaResolveCode::UpstreamFailure, true),
            (400, MediaResolveCode::InvalidRequest, false),
        ];
        for (status, code, retryable) in cases {
            let error = MediaResolveError::from_http_status(status, None);
            assert_eq!(error.code, code, "status {status}");
            assert_eq!(error.retryable, retryable, "status {status}");
            assert_eq!(
                error.detail_code.as_deref(),
                Some(format!("UPSTREAM_{status}").as_str())
            );
        }
    }

    #[test]
    fn reason_is_the_code_string() {
        assert_eq!(
            MediaResolveError::content_unavailable("NO_STREAMS").reason(),
            "CONTENT_UNAVAILABLE"
        );
    }
}
