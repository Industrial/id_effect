//! Map sqlx failures into [`SqlError`](id_effect_sql::SqlError).

use id_effect_sql::SqlError;

/// Driver-local error wrapper (mostly for pool construction).
#[derive(Debug)]
pub struct PgSqlError(pub SqlError);

impl PgSqlError {
  #[inline]
  pub(crate) fn from_sqlx(err: sqlx::Error) -> Self {
    Self(sqlx_error(err))
  }
}

pub(crate) fn sqlx_error(err: sqlx::Error) -> SqlError {
  match err {
    sqlx::Error::PoolClosed | sqlx::Error::PoolTimedOut => SqlError::NotConnected,
    sqlx::Error::Configuration(msg) => SqlError::Unsupported(msg.to_string()),
    other => SqlError::QueryFailed {
      sql: String::new(),
      message: other.to_string(),
    },
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect_sql::SqlError;

  #[test]
  fn pool_closed_maps_to_not_connected() {
    let err = sqlx_error(sqlx::Error::PoolClosed);
    assert!(matches!(err, SqlError::NotConnected));
  }
}
