//! [`SqliteSqlTransaction`] — statements, savepoints, commit and rollback over a sqlx
//! [`Transaction`].

use std::sync::Arc;

use id_effect::{Effect, fail, from_async};
use sqlx::{Executor as _, Sqlite, Transaction};
use tokio::sync::Mutex;

use super::error::sqlite_error;
use super::value::{bind_query, decode_rows};
use crate::transaction::savepoint_sql;
use crate::{SqlError, SqlRow, SqlTransaction, SqlValue};

type Slot = Arc<Mutex<Option<Transaction<'static, Sqlite>>>>;

/// SQLite transaction handle: its statements run on the transaction's connection.
///
/// It wraps a sqlx [`Transaction`], so a handle dropped without `commit` / `rollback` (or a
/// `COMMIT` cut short) is rolled back by sqlx before its connection is reused.
pub struct SqliteSqlTransaction {
  tx: Slot,
}

impl SqliteSqlTransaction {
  pub(crate) fn new(tx: Transaction<'static, Sqlite>) -> Self {
    Self {
      tx: Arc::new(Mutex::new(Some(tx))),
    }
  }

  /// Runs a savepoint statement unprepared: savepoint names are generated, so caching their
  /// statements would only fill the statement cache.
  fn savepoint_statement(&self, verb: &str, name: &str) -> Effect<(), SqlError> {
    let sql = match savepoint_sql(verb, name) {
      Ok(sql) => sql,
      Err(e) => return fail(e),
    };
    let slot = Arc::clone(&self.tx);
    from_async(move || async move {
      let mut guard = slot.lock().await;
      let tx = guard.as_mut().ok_or_else(already_finished)?;
      (&mut **tx)
        .execute(sql.as_str())
        .await
        .map_err(sqlite_error)?;
      Ok(())
    })
  }
}

fn already_finished() -> SqlError {
  SqlError::TransactionFailed("transaction already finished".into())
}

async fn take(slot: &Slot) -> Result<Transaction<'static, Sqlite>, SqlError> {
  slot.lock().await.take().ok_or_else(already_finished)
}

impl SqlTransaction for SqliteSqlTransaction {
  fn query(&self, sql: &str, params: &[SqlValue]) -> Effect<Vec<SqlRow>, SqlError> {
    let (slot, sql, params) = (Arc::clone(&self.tx), sql.to_string(), params.to_vec());
    from_async(move || async move {
      let mut guard = slot.lock().await;
      let tx = guard.as_mut().ok_or_else(already_finished)?;
      let rows = bind_query(sqlx::query(&sql), &params)?
        .fetch_all(&mut **tx)
        .await
        .map_err(sqlite_error)?;
      decode_rows(&rows)
    })
  }

  fn execute(&self, sql: &str, params: &[SqlValue]) -> Effect<u64, SqlError> {
    let (slot, sql, params) = (Arc::clone(&self.tx), sql.to_string(), params.to_vec());
    from_async(move || async move {
      let mut guard = slot.lock().await;
      let tx = guard.as_mut().ok_or_else(already_finished)?;
      let result = bind_query(sqlx::query(&sql), &params)?
        .execute(&mut **tx)
        .await
        .map_err(sqlite_error)?;
      Ok(result.rows_affected())
    })
  }

  fn commit(&self) -> Effect<(), SqlError> {
    let slot = Arc::clone(&self.tx);
    from_async(move || async move { take(&slot).await?.commit().await.map_err(sqlite_error) })
  }

  fn rollback(&self) -> Effect<(), SqlError> {
    let slot = Arc::clone(&self.tx);
    from_async(move || async move { take(&slot).await?.rollback().await.map_err(sqlite_error) })
  }

  fn create_savepoint(&self, name: &str) -> Effect<(), SqlError> {
    self.savepoint_statement("SAVEPOINT", name)
  }

  fn release_savepoint(&self, name: &str) -> Effect<(), SqlError> {
    self.savepoint_statement("RELEASE SAVEPOINT", name)
  }

  fn rollback_to_savepoint(&self, name: &str) -> Effect<(), SqlError> {
    self.savepoint_statement("ROLLBACK TO SAVEPOINT", name)
  }
}
