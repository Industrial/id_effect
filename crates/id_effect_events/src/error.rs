//! Event store and CQRS errors.

use id_effect_sql::SqlError;

/// Failure while reading or writing events.
///
/// Non-exhaustive: match it with a wildcard arm.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum EventStoreError {
  /// Stream does not exist.
  #[error("stream `{stream_id}` not found")]
  StreamNotFound {
    /// Stream identifier.
    stream_id: String,
  },
  /// Optimistic concurrency conflict: the stream was not at the version the writer expected
  /// ([`EventStore::append_expected`](crate::EventStore::append_expected)), or another writer
  /// appended the same version first.
  #[error("expected version {expected}, found {actual} on stream `{stream_id}`")]
  VersionConflict {
    /// Stream identifier.
    stream_id: String,
    /// The stream version the writer expected (`0`: an empty stream).
    expected: u64,
    /// The stream version found.
    actual: u64,
  },
  /// The SQL journal's database failed ([`SqlEventJournal`](crate::SqlEventJournal)).
  #[error("sql error: {0}")]
  Sql(#[from] SqlError),
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
      (
        EventStoreError::from(SqlError::NotConnected),
        "sql error: sql client not connected",
      ),
    ];
    for (err, expected) in cases {
      assert_eq!(err.to_string(), expected);
    }
  }
}
