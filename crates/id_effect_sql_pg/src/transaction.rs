//! [`PgSqlTransaction`] — commit/rollback over a pooled sqlx connection.

use std::sync::{Arc, Mutex};

use id_effect::kernel::Effect;
use id_effect_sql::{SqlError, SqlTransaction};
use sqlx::Postgres;
use sqlx::pool::PoolConnection;

use crate::error::sqlx_error;

struct PgSqlTransactionInner {
  conn: Mutex<Option<PoolConnection<Postgres>>>,
  finished: Mutex<bool>,
}

/// PostgreSQL transaction handle (explicit `COMMIT` / `ROLLBACK` on the held connection).
pub struct PgSqlTransaction {
  inner: Arc<PgSqlTransactionInner>,
}

impl PgSqlTransaction {
  pub(crate) fn new(conn: PoolConnection<Postgres>) -> Self {
    Self {
      inner: Arc::new(PgSqlTransactionInner {
        conn: Mutex::new(Some(conn)),
        finished: Mutex::new(false),
      }),
    }
  }

  fn take_conn(inner: &PgSqlTransactionInner) -> Result<PoolConnection<Postgres>, SqlError> {
    let mut finished = inner.finished.lock().expect("tx finished mutex poisoned");
    if *finished {
      return Err(SqlError::TransactionFailed(
        "transaction already finished".into(),
      ));
    }
    *finished = true;
    drop(finished);
    let mut guard = inner.conn.lock().expect("tx conn mutex poisoned");
    guard
      .take()
      .ok_or_else(|| SqlError::TransactionFailed("connection already released".into()))
  }
}

impl SqlTransaction for PgSqlTransaction {
  fn commit(&self) -> Effect<(), SqlError, ()> {
    let inner = Arc::clone(&self.inner);
    Effect::new_async(move |_r: &mut ()| {
      Box::pin(async move {
        let mut conn = Self::take_conn(&inner)?;
        sqlx::query("COMMIT")
          .execute(&mut *conn)
          .await
          .map_err(sqlx_error)?;
        Ok(())
      })
    })
  }

  fn rollback(&self) -> Effect<(), SqlError, ()> {
    let inner = Arc::clone(&self.inner);
    Effect::new_async(move |_r: &mut ()| {
      Box::pin(async move {
        let mut conn = Self::take_conn(&inner)?;
        sqlx::query("ROLLBACK")
          .execute(&mut *conn)
          .await
          .map_err(sqlx_error)?;
        Ok(())
      })
    })
  }
}
