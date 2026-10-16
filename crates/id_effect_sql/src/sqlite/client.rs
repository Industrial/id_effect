//! [`SqliteSqlClient`] — [`SqlClient`] over sqlx [`SqlitePool`].

use std::sync::Arc;

use id_effect::Never;
use id_effect::{Effect, Layer, fail, from_async, hlist};
use sqlx::SqlitePool;

use super::config::{SqlitePoolConfig, sqlite_pool_from_config};
use super::error::sqlite_error;
use super::transaction::SqliteSqlTransaction;
use super::value::{bind_query, decode_rows};
use crate::{
  Isolation, SqlClient, SqlClientEnv, SqlClientService, SqlError, SqlRow, SqlTransaction, SqlValue,
  TransactionOptions,
};

/// SQLite [`SqlClient`] backed by a sqlx connection pool.
#[derive(Clone)]
pub struct SqliteSqlClient {
  pool: SqlitePool,
}

impl SqliteSqlClient {
  /// Wrap an existing sqlx [`SqlitePool`].
  #[inline]
  pub fn new(pool: SqlitePool) -> Self {
    Self { pool }
  }

  /// Borrow the underlying pool.
  #[inline]
  pub fn pool(&self) -> &SqlitePool {
    &self.pool
  }

  /// The scoped SQLite client [`Layer`] (Effect.ts `Layer.scoped`): building it opens a pool
  /// from `config` and checks it with `SELECT 1` (a failure fails the build with
  /// `E::from(err)`); the build's scope closing closes the pool (and with it an in-memory
  /// database). It provides the client as [`SqlClientService`].
  pub fn layer<E>(config: SqlitePoolConfig) -> Layer<SqlClientEnv, E>
  where
    E: From<SqlError> + Send + 'static,
  {
    Layer::acquire_release(
      move || {
        let config = config.clone();
        from_async(move || async move {
          let pool = sqlite_pool_from_config(config).await.map_err(|e| e.0)?;
          ping(&pool).await?;
          Ok::<_, SqlError>(pool)
        })
        .map_err(E::from)
        .map(|pool: SqlitePool| hlist![SqlClientService::new(SqliteSqlClient::new(pool))])
      },
      |env: SqlClientEnv, _kind| env.0.close(),
    )
  }
}

async fn ping(pool: &SqlitePool) -> Result<(), SqlError> {
  sqlx::query("SELECT 1")
    .execute(pool)
    .await
    .map_err(sqlite_error)?;
  Ok(())
}

impl SqlClient for SqliteSqlClient {
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
        .map_err(sqlite_error)?;
      decode_rows(&rows)
    })
  }

  fn execute(&self, sql: &str, params: &[SqlValue]) -> Effect<u64, SqlError> {
    let (pool, sql, params) = (self.pool.clone(), sql.to_string(), params.to_vec());
    from_async(move || async move {
      let result = bind_query(sqlx::query(&sql), &params)?
        .execute(&pool)
        .await
        .map_err(sqlite_error)?;
      Ok(result.rows_affected())
    })
  }

  fn begin(&self) -> Effect<Arc<dyn SqlTransaction>, SqlError> {
    self.begin_with(TransactionOptions::new())
  }

  /// `BEGIN IMMEDIATE` for [`Isolation::Serializable`], `BEGIN` (deferred) otherwise: SQLite
  /// runs every transaction serializably. `READ ONLY` is [`SqlError::Unsupported`];
  /// `DEFERRABLE` changes nothing (see the [module docs](super)).
  fn begin_with(&self, options: TransactionOptions) -> Effect<Arc<dyn SqlTransaction>, SqlError> {
    if options.read_only() {
      return fail(SqlError::Unsupported(
        "SQLite has no READ ONLY transactions; open the database read-only (`?mode=ro`)".into(),
      ));
    }
    let statement = match options.isolation() {
      Some(Isolation::Serializable) => "BEGIN IMMEDIATE",
      _ => "BEGIN",
    };
    let pool = self.pool.clone();
    from_async(move || async move {
      let tx = pool.begin_with(statement).await.map_err(sqlite_error)?;
      Ok(Arc::new(SqliteSqlTransaction::new(tx)) as Arc<dyn SqlTransaction>)
    })
  }

  /// Streams the rows from a sqlx `fetch` on a pooled connection, buffering at most two batches
  /// of 64 rows; dropping or interrupting the stream resets the statement.
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
