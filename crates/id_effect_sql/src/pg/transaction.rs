//! [`PgSqlTransaction`] — statements, commit and rollback over a sqlx [`Transaction`].

use std::sync::Arc;

use id_effect::{Effect, from_async};
use sqlx::{Postgres, Transaction};
use tokio::sync::Mutex;

use super::client::{bind_query, row_to_sql_row};
use super::error::sqlx_error;
use crate::{SqlError, SqlParam, SqlRow, SqlTransaction};

type Slot = Arc<Mutex<Option<Transaction<'static, Postgres>>>>;

/// PostgreSQL transaction handle: its statements run on the transaction's connection.
///
/// It wraps a sqlx [`Transaction`], so a handle dropped without `commit` / `rollback` (or a
/// `COMMIT` cut short) is rolled back by sqlx when its connection returns to the pool.
pub struct PgSqlTransaction {
  tx: Slot,
}

impl PgSqlTransaction {
  pub(crate) fn new(tx: Transaction<'static, Postgres>) -> Self {
    Self {
      tx: Arc::new(Mutex::new(Some(tx))),
    }
  }
}

fn already_finished() -> SqlError {
  SqlError::TransactionFailed("transaction already finished".into())
}

async fn take(slot: &Slot) -> Result<Transaction<'static, Postgres>, SqlError> {
  slot.lock().await.take().ok_or_else(already_finished)
}

impl SqlTransaction for PgSqlTransaction {
  fn query(&self, sql: &str, params: &[SqlParam]) -> Effect<Vec<SqlRow>, SqlError> {
    let (slot, sql, params) = (Arc::clone(&self.tx), sql.to_string(), params.to_vec());
    from_async(move || async move {
      let mut guard = slot.lock().await;
      let tx = guard.as_mut().ok_or_else(already_finished)?;
      let rows = bind_query(sqlx::query(&sql), &params)
        .fetch_all(&mut **tx)
        .await
        .map_err(sqlx_error)?;
      Ok(rows.iter().map(row_to_sql_row).collect())
    })
  }

  fn execute(&self, sql: &str, params: &[SqlParam]) -> Effect<u64, SqlError> {
    let (slot, sql, params) = (Arc::clone(&self.tx), sql.to_string(), params.to_vec());
    from_async(move || async move {
      let mut guard = slot.lock().await;
      let tx = guard.as_mut().ok_or_else(already_finished)?;
      let result = bind_query(sqlx::query(&sql), &params)
        .execute(&mut **tx)
        .await
        .map_err(sqlx_error)?;
      Ok(result.rows_affected())
    })
  }

  fn commit(&self) -> Effect<(), SqlError> {
    let slot = Arc::clone(&self.tx);
    from_async(move || async move { take(&slot).await?.commit().await.map_err(sqlx_error) })
  }

  fn rollback(&self) -> Effect<(), SqlError> {
    let slot = Arc::clone(&self.tx);
    from_async(move || async move { take(&slot).await?.rollback().await.map_err(sqlx_error) })
  }
}
