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
  /// Projection graph planning failure.
  #[error("graph error: {0}")]
  Graph(String),
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn display_formats_all_variants() {
    let cases = [
      (
        EventStoreError::StreamNotFound {
          stream_id: "s".into(),
        },
        "stream `s` not found",
      ),
      (
        EventStoreError::VersionConflict {
          stream_id: "s".into(),
          expected: 2,
          actual: 3,
        },
        "expected version 2, found 3 on stream `s`",
      ),
      (EventStoreError::Io("disk".into()), "io error: disk"),
      (
        EventStoreError::Serde("bad json".into()),
        "serde error: bad json",
      ),
      (
        EventStoreError::Schema("bad field".into()),
        "schema error: bad field",
      ),
      (EventStoreError::Graph("cycle".into()), "graph error: cycle"),
    ];
    for (err, expected) in cases {
      assert_eq!(err.to_string(), expected);
    }
  }
}
