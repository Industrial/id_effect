//! Map sqlx SQLite failures into [`SqlError`](crate::SqlError).

use crate::SqlError;
use crate::transaction::{SQLSTATE_SERIALIZATION_FAILURE, with_sqlstate};

/// Driver-local error wrapper (mostly for pool construction).
#[derive(Debug)]
pub struct SqliteSqlError(pub SqlError);

impl SqliteSqlError {
  #[inline]
  pub(crate) fn from_sqlx(err: sqlx::Error) -> Self {
    Self(sqlite_error(err))
  }
}

/// Whether an (extended) SQLite result code is `SQLITE_BUSY` or one of its extensions
/// (`SQLITE_BUSY_SNAPSHOT`: a WAL read transaction cannot be upgraded to write).
fn is_busy(code: &str) -> bool {
  code.parse::<i32>().is_ok_and(|code| code & 0xff == 5)
}

/// The [`SqlError`] of a sqlx failure. A busy database is tagged SQLSTATE `40001`: like a
/// serialization failure, the transaction lost a conflict and running it again may succeed.
pub(crate) fn sqlite_error(err: sqlx::Error) -> SqlError {
  match err {
    sqlx::Error::PoolClosed | sqlx::Error::PoolTimedOut => SqlError::NotConnected,
    sqlx::Error::Configuration(msg) => SqlError::Unsupported(msg.to_string()),
    other => {
      let busy = other
        .as_database_error()
        .and_then(|db| db.code())
        .is_some_and(|code| is_busy(&code));
      let message = other.to_string();
      SqlError::QueryFailed {
        sql: String::new(),
        message: if busy {
          with_sqlstate(message, SQLSTATE_SERIALIZATION_FAILURE)
        } else {
          message
        },
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn pool_and_configuration_errors_map_to_their_variants() {
    assert_eq!(
      sqlite_error(sqlx::Error::PoolClosed),
      SqlError::NotConnected
    );
    assert_eq!(
      sqlite_error(sqlx::Error::PoolTimedOut),
      SqlError::NotConnected
    );
    assert!(matches!(
      sqlite_error(sqlx::Error::Configuration("bad".into())),
      SqlError::Unsupported(m) if m == "bad"
    ));
    assert!(matches!(
      sqlite_error(sqlx::Error::RowNotFound),
      SqlError::QueryFailed { message, .. } if !message.contains("SQLSTATE")
    ));
  }

  #[test]
  fn busy_codes_are_recognised() {
    for code in ["5", "261", "517", "773"] {
      assert!(is_busy(code), "{code}");
    }
    for code in ["1", "6", "262", "x"] {
      assert!(!is_busy(code), "{code}");
    }
  }
}
