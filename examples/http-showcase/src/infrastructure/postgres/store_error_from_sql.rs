//! `impl From<SqlError> for StoreError`: how a SQL failure surfaces through the
//! [`TodoRepository`](crate::domain::todo::TodoRepository) port.

use id_effect_sql::SqlError;

use crate::domain::todo::StoreError;

/// An unreachable store ([`SqlError::NotConnected`]: the pool is closed or timed out, no
/// statement ran) is [`StoreError::Unavailable`], so retries may help; anything else is
/// [`StoreError::Internal`]. A unique violation is a domain outcome, not a store failure: the
/// repository maps it first (`UniqueViolation`).
impl From<SqlError> for StoreError {
  fn from(err: SqlError) -> Self {
    match err {
      SqlError::NotConnected => Self::Unavailable(err.to_string()),
      other => Self::Internal(other.to_string()),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn only_not_connected_is_unavailable() {
    assert!(StoreError::from(SqlError::NotConnected).is_transient());
    let others = [
      SqlError::QueryFailed {
        sql: "SELECT 1".into(),
        message: "boom".into(),
      },
      SqlError::TransactionFailed("commit".into()),
      SqlError::Decode("bad cell".into()),
      SqlError::Unsupported("x".into()),
    ];
    for err in others {
      assert!(
        matches!(StoreError::from(err.clone()), StoreError::Internal(_)),
        "{err:?}"
      );
    }
  }
}
