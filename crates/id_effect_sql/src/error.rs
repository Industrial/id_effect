//! Structured errors for SQL client operations.

use std::fmt;

/// SQL client / query failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SqlError {
  /// Client is not connected (or pool unavailable).
  NotConnected,
  /// Parameterized query failed to execute.
  QueryFailed {
    /// SQL text (may be redacted in logs at higher layers).
    sql: String,
    /// Human-readable driver or test-double message.
    message: String,
  },
  /// Transaction could not begin, commit, or roll back.
  TransactionFailed(String),
  /// Row or column could not be decoded.
  Decode(String),
  /// Operation is unsupported by the current implementation.
  Unsupported(String),
}

impl fmt::Display for SqlError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      SqlError::NotConnected => write!(f, "sql client not connected"),
      SqlError::QueryFailed { sql, message } => {
        write!(f, "sql query failed ({sql}): {message}")
      }
      SqlError::TransactionFailed(msg) => write!(f, "sql transaction failed: {msg}"),
      SqlError::Decode(msg) => write!(f, "sql decode error: {msg}"),
      SqlError::Unsupported(msg) => write!(f, "sql unsupported: {msg}"),
    }
  }
}

impl std::error::Error for SqlError {}

#[cfg(test)]
mod tests {
  use super::SqlError;

  #[test]
  fn display_includes_query_context() {
    let err = SqlError::QueryFailed {
      sql: "SELECT 1".into(),
      message: "boom".into(),
    };
    assert!(err.to_string().contains("SELECT 1"));
    assert!(err.to_string().contains("boom"));
  }

  #[test]
  fn display_all_variants() {
    assert_eq!(
      SqlError::NotConnected.to_string(),
      "sql client not connected"
    );
    assert!(
      SqlError::TransactionFailed("tx".into())
        .to_string()
        .contains("tx")
    );
    assert!(SqlError::Decode("col".into()).to_string().contains("col"));
    assert!(
      SqlError::Unsupported("nope".into())
        .to_string()
        .contains("nope")
    );
  }
}
