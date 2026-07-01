//! Error type for the transport layer.

/// Errors raised while sending on or driving a Cast connection.
#[derive(Debug, thiserror::Error)]
pub enum CastError {
    /// The peer connection has been closed (writer task gone).
    #[error("connection closed")]
    Closed,
    /// Transport I/O failure.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// Message framing failure.
    #[error(transparent)]
    Framing(#[from] vibecast_proto::FramingError),
    /// JSON payload serialization failure.
    #[error("json serialization error: {0}")]
    Json(#[from] serde_json::Error),
}
