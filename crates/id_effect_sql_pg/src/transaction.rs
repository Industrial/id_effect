//! [`PgSqlTransaction`] — commit/rollback over a pooled connection with `BEGIN`.

use std::sync::{Arc, Mutex};

use deadpool_postgres::Object;
use id_effect::kernel::Effect;
use id_effect_sql::{SqlError, SqlTransaction};

use crate::error::pg_error;

struct PgSqlTransactionInner {
  client: Mutex<Option<Object>>,
  finished: Mutex<bool>,
}

/// PostgreSQL transaction handle (explicit `COMMIT` / `ROLLBACK` on the held connection).
pub struct PgSqlTransaction {
  inner: Arc<PgSqlTransactionInner>,
}

impl PgSqlTransaction {
  pub(crate) fn new(client: Object) -> Self {
    Self {
      inner: Arc::new(PgSqlTransactionInner {
        client: Mutex::new(Some(client)),
        finished: Mutex::new(false),
      }),
    }
  }

  fn take_client(inner: &PgSqlTransactionInner) -> Result<Object, SqlError> {
    let mut finished = inner.finished.lock().expect("tx finished mutex poisoned");
    if *finished {
      return Err(SqlError::TransactionFailed(
        "transaction already finished".into(),
      ));
    }
    *finished = true;
    drop(finished);
    let mut guard = inner.client.lock().expect("tx client mutex poisoned");
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
        let client = Self::take_client(&inner)?;
        client.execute("COMMIT", &[]).await.map_err(pg_error)?;
        Ok(())
      })
    })
  }

  fn rollback(&self) -> Effect<(), SqlError, ()> {
    let inner = Arc::clone(&self.inner);
    Effect::new_async(move |_r: &mut ()| {
      Box::pin(async move {
        let client = Self::take_client(&inner)?;
        client.execute("ROLLBACK", &[]).await.map_err(pg_error)?;
        Ok(())
      })
    })
  }
}
