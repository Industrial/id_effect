//! [`MySqlSqlClient`] — [`SqlClient`] over sqlx [`MySqlPool`].

use std::sync::Arc;

use id_effect::Never;
use id_effect::{Effect, Layer, from_async, hlist};
use sqlx::MySqlPool;

use super::config::{MySqlPoolConfig, mysql_pool_from_config};
use super::error::mysql_error;
use super::transaction::MySqlSqlTransaction;
use super::value::{bind_query, decode_rows};
use crate::{
  SqlClient, SqlClientEnv, SqlClientService, SqlError, SqlRow, SqlTransaction, SqlValue,
  TransactionOptions,
};

/// MySQL [`SqlClient`] backed by a sqlx connection pool.
#[derive(Clone)]
pub struct MySqlSqlClient {
  pool: MySqlPool,
}

impl MySqlSqlClient {
  /// Wrap an existing sqlx [`MySqlPool`]. Its connections should use the UTC session time zone
  /// (sqlx's default; [`MySqlPoolConfig`] forces it), or `TIMESTAMP` values read and bind
  /// shifted.
  #[inline]
  pub fn new(pool: MySqlPool) -> Self {
    Self { pool }
  }

  /// Borrow the underlying pool.
  #[inline]
  pub fn pool(&self) -> &MySqlPool {
    &self.pool
  }

  /// The scoped MySQL client [`Layer`] (Effect.ts `Layer.scoped`): building it opens a pool from
  /// `config` and checks it with `SELECT 1` (a failure fails the build with `E::from(err)`); the
  /// build's scope closing closes the pool. It provides the client as [`SqlClientService`].
  pub fn layer<E>(config: MySqlPoolConfig) -> Layer<SqlClientEnv, E>
  where
    E: From<SqlError> + Send + 'static,
  {
    Layer::acquire_release(
      move || {
        let config = config.clone();
        from_async(move || async move {
          let pool = mysql_pool_from_config(config).await.map_err(|e| e.0)?;
          ping(&pool).await?;
          Ok::<_, SqlError>(pool)
        })
        .map_err(E::from)
        .map(|pool: MySqlPool| hlist![SqlClientService::new(MySqlSqlClient::new(pool))])
      },
      |env: SqlClientEnv, _kind| env.0.close(),
    )
  }
}

async fn ping(pool: &MySqlPool) -> Result<(), SqlError> {
  sqlx::query("SELECT 1")
    .execute(pool)
    .await
    .map_err(mysql_error)?;
  Ok(())
}

/// The statement beginning a transaction with `options`: MySQL sets the isolation level of the
/// next transaction *before* it starts, so `SET TRANSACTION ISOLATION LEVEL …` comes first, in
/// the same round trip. `DEFERRABLE` has no MySQL equivalent and changes nothing.
fn begin_sql(options: TransactionOptions) -> String {
  let start = if options.read_only() {
    "START TRANSACTION READ ONLY"
  } else {
    "START TRANSACTION"
  };
  match options.isolation() {
    Some(level) => format!(
      "SET TRANSACTION ISOLATION LEVEL {}; {start}",
      level.as_sql()
    ),
    None => start.to_owned(),
  }
}

impl SqlClient for MySqlSqlClient {
  fn connect(&self) -> Effect<(), SqlError> {
    let pool = self.pool.clone();
    from_async(move || async move { ping(&pool).await })
  }

  fn query(&self, sql: &str, params: &[SqlValue]) -> Effect<Vec<SqlRow>, SqlError> {
    let (pool, sql, params) = (self.pool.clone(), sql.to_string(), params.to_vec());
    from_async(move || async move {
      let rows = bind_query(sqlx::query(&sql), &params)?
        .fetch_all(&pool)
        .await
        .map_err(mysql_error)?;
      decode_rows(&rows)
    })
  }

  fn execute(&self, sql: &str, params: &[SqlValue]) -> Effect<u64, SqlError> {
    let (pool, sql, params) = (self.pool.clone(), sql.to_string(), params.to_vec());
    from_async(move || async move {
      let result = bind_query(sqlx::query(&sql), &params)?
        .execute(&pool)
        .await
        .map_err(mysql_error)?;
      Ok(result.rows_affected())
    })
  }

  fn begin(&self) -> Effect<Arc<dyn SqlTransaction>, SqlError> {
    self.begin_with(TransactionOptions::new())
  }

  /// `START TRANSACTION [READ ONLY]`, preceded by `SET TRANSACTION ISOLATION LEVEL …` when a
  /// level is asked for (see the [module docs](super)).
  fn begin_with(&self, options: TransactionOptions) -> Effect<Arc<dyn SqlTransaction>, SqlError> {
    let (pool, statement) = (self.pool.clone(), begin_sql(options));
    from_async(move || async move {
      let tx = pool.begin_with(statement).await.map_err(mysql_error)?;
      Ok(Arc::new(MySqlSqlTransaction::new(tx)) as Arc<dyn SqlTransaction>)
    })
  }

  /// Streams the rows from a sqlx `fetch` on a pooled connection, buffering at most two batches
  /// of 64 rows; dropping or interrupting the stream stops the query with `KILL QUERY`.
  fn query_stream(&self, sql: &str, params: &[SqlValue]) -> id_effect::Stream<SqlRow, SqlError> {
    super::stream::query_stream(self.pool.clone(), sql.to_owned(), params.to_vec())
  }

  fn close(&self) -> Effect<(), Never> {
    let pool = self.pool.clone();
    from_async(move || async move {
      pool.close().await;
      Ok(())
    })
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::Isolation;

  #[test]
  fn begin_statements_set_the_level_first() {
    assert_eq!(begin_sql(TransactionOptions::new()), "START TRANSACTION");
    assert_eq!(
      begin_sql(TransactionOptions::new().with_read_only().with_deferrable()),
      "START TRANSACTION READ ONLY"
    );
    assert_eq!(
      begin_sql(TransactionOptions::from(Isolation::ReadCommitted).with_read_only()),
      "SET TRANSACTION ISOLATION LEVEL READ COMMITTED; START TRANSACTION READ ONLY"
    );
  }
}
