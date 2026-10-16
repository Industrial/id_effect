//! Map sqlx MySQL failures into [`SqlError`](crate::SqlError).

use sqlx::error::DatabaseError;
use sqlx::mysql::MySqlDatabaseError;

use crate::SqlError;
use crate::transaction::{SQLSTATE_SERIALIZATION_FAILURE, with_sqlstate};

/// Driver-local error wrapper (mostly for pool construction).
#[derive(Debug)]
pub struct MySqlSqlError(pub SqlError);

impl MySqlSqlError {
  #[inline]
  pub(crate) fn from_sqlx(err: sqlx::Error) -> Self {
    Self(mysql_error(err))
  }
}

/// `ER_LOCK_WAIT_TIMEOUT`: a statement gave up waiting for a row lock (SQLSTATE `HY000`).
const ER_LOCK_WAIT_TIMEOUT: u16 = 1205;

/// The SQLSTATE a server error is tagged with: its own (a deadlock, `ER_LOCK_DEADLOCK` 1213, is
/// already `40001`), except that a lock wait timeout is `40001` too.
fn sqlstate(db: &dyn DatabaseError) -> Option<String> {
  let mysql = db.try_downcast_ref::<MySqlDatabaseError>()?;
  if mysql.number() == ER_LOCK_WAIT_TIMEOUT {
    return Some(SQLSTATE_SERIALIZATION_FAILURE.to_owned());
  }
  mysql.code().map(str::to_owned)
}

/// The [`SqlError`] of a sqlx failure. A server error's message ends with its SQLSTATE
/// (`"… (SQLSTATE 40001)"`); a deadlock and a lock wait timeout are both `40001`, so
/// [`retry_serialization_failures`](crate::retry_serialization_failures) retries them.
pub(crate) fn mysql_error(err: sqlx::Error) -> SqlError {
  match err {
    sqlx::Error::PoolClosed | sqlx::Error::PoolTimedOut => SqlError::NotConnected,
    sqlx::Error::Configuration(msg) => SqlError::Unsupported(msg.to_string()),
    other => {
      let code = other.as_database_error().and_then(sqlstate);
      let message = other.to_string();
      SqlError::QueryFailed {
        sql: String::new(),
        message: match code {
          Some(code) => with_sqlstate(message, &code),
          None => message,
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
    assert_eq!(mysql_error(sqlx::Error::PoolClosed), SqlError::NotConnected);
    assert_eq!(
      mysql_error(sqlx::Error::PoolTimedOut),
      SqlError::NotConnected
    );
    assert!(matches!(
      MySqlSqlError::from_sqlx(sqlx::Error::Configuration("bad".into())).0,
      SqlError::Unsupported(m) if m == "bad"
    ));
    assert!(matches!(
      mysql_error(sqlx::Error::RowNotFound),
      SqlError::QueryFailed { message, .. } if !message.contains("SQLSTATE")
    ));
  }
}
