//! Map deadpool / tokio-postgres failures into [`SqlError`](id_effect_sql::SqlError).

use id_effect_sql::SqlError;

/// Driver-local error wrapper (mostly for pool construction).
#[derive(Debug)]
pub struct PgSqlError(pub SqlError);

impl From<deadpool_postgres::CreatePoolError> for PgSqlError {
  fn from(err: deadpool_postgres::CreatePoolError) -> Self {
    Self(SqlError::Unsupported(err.to_string()))
  }
}

impl PgSqlError {
  #[inline]
  pub(crate) fn from_build(err: deadpool_postgres::BuildError) -> Self {
    Self(SqlError::Unsupported(err.to_string()))
  }
}

pub(crate) fn pool_error(err: deadpool_postgres::PoolError) -> SqlError {
  match err {
    deadpool_postgres::PoolError::Backend(e) => SqlError::QueryFailed {
      sql: String::new(),
      message: e.to_string(),
    },
    _ => SqlError::NotConnected,
  }
}

pub(crate) fn pg_error(err: tokio_postgres::Error) -> SqlError {
  SqlError::QueryFailed {
    sql: String::new(),
    message: err.to_string(),
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect_sql::SqlError;

  #[test]
  fn pool_error_non_backend_is_not_connected() {
    let err = pool_error(deadpool_postgres::PoolError::Closed);
    assert!(matches!(err, SqlError::NotConnected));
  }

  #[test]
  fn pg_error_maps_to_query_failed() {
    let err = pg_error(tokio_postgres::Error::__private_api_timeout());
    assert!(matches!(err, SqlError::QueryFailed { .. }));
    assert!(!err.to_string().is_empty());
  }

  #[test]
  fn pg_sql_error_from_create_pool_error() {
    let build_err = deadpool_postgres::Config::new()
      .create_pool(None, tokio_postgres::NoTls)
      .unwrap_err();
    let mapped = PgSqlError::from(build_err);
    assert!(matches!(mapped.0, SqlError::Unsupported(_)));
  }
}
