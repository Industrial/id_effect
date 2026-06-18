//! Event store and CQRS errors.

/// Failure while reading or writing events.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum EventStoreError {
  /// Stream does not exist.
  #[error("stream `{stream_id}` not found")]
  StreamNotFound {
    /// Stream identifier.
    stream_id: String,
  },
  /// Optimistic concurrency conflict.
  #[error("expected version {expected}, found {actual} on stream `{stream_id}`")]
  VersionConflict {
    /// Stream identifier.
    stream_id: String,
    /// Expected next version.
    expected: u64,
    /// Actual next version.
    actual: u64,
  },
  /// Underlying I/O failure.
  #[error("io error: {0}")]
  Io(String),
  /// JSON encode/decode failure.
  #[error("serde error: {0}")]
  Serde(String),
  /// Schema decode failure.
  #[error("schema error: {0}")]
  Schema(String),
}
