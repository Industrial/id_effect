//! [`PgSqlClient`] — [`SqlClient`](crate::SqlClient) over sqlx [`PgPool`].

use std::collections::BTreeMap;
use std::sync::Arc;

use id_effect::Never;
use id_effect::{Effect, Layer, from_async, hlist};
use sqlx::PgPool;

use super::config::{PgPoolConfig, pg_pool_from_config};
use super::error::sqlx_error;
use super::pool_key::PgPool as PgPoolCap;
use super::providers::PgSqlEnv;
use super::transaction::PgSqlTransaction;
use super::value::{bind_query, decode_rows};
use crate::{SqlClient, SqlClientService, SqlError, SqlRow, SqlTransaction, SqlValue};

/// PostgreSQL [`SqlClient`] backed by a sqlx connection pool.
#[derive(Clone)]
pub struct PgSqlClient {
  pool: PgPool,
}

impl PgSqlClient {
  /// Wrap an existing sqlx [`PgPool`].
  #[inline]
  pub fn new(pool: PgPool) -> Self {
    Self { pool }
  }

  /// Borrow the underlying pool (for Apalis, obix, and other adapters).
  #[inline]
  pub fn pool(&self) -> &PgPool {
    &self.pool
  }

  /// Pool health check: runs `SELECT 1` on a pooled connection, then reports the pool's `size`
  /// (open connections), `idle` connections and `max_size`. Fails (down) with the [`SqlError`]
  /// when no connection can be acquired or the query fails.
  pub fn pool_health(&self) -> Effect<BTreeMap<&'static str, u64>, SqlError> {
    let pool = self.pool.clone();
    from_async(move || async move {
      ping(&pool).await?;
      Ok(BTreeMap::from([
        ("size", u64::from(pool.size())),
        ("idle", u64::try_from(pool.num_idle()).unwrap_or(u64::MAX)),
        ("max_size", u64::from(pool.options().get_max_connections())),
      ]))
    })
  }

  /// The scoped PostgreSQL client [`Layer`] (Effect.ts `Layer.scoped`): building it opens a pool
  /// from `config` and checks it with `SELECT 1` (a failure fails the build with
  /// `E::from(err)`); the build's scope closing closes the pool. It provides the client as
  /// [`SqlClientService`] and the pool as [`PgPool`](super::PgPool) ([`PgSqlEnv`]).
  pub fn layer<E>(config: PgPoolConfig) -> Layer<PgSqlEnv, E>
  where
    E: From<SqlError> + Send + 'static,
  {
    Layer::acquire_release(
      move || {
        let config = config.clone();
        from_async(move || async move {
          let pool = pg_pool_from_config(config).await.map_err(|e| e.0)?;
          ping(&pool).await?;
          Ok::<_, SqlError>(pool)
        })
        .map_err(E::from)
        .map(|pool: PgPool| {
          hlist![
            SqlClientService::new(PgSqlClient::new(pool.clone())),
            PgPoolCap(pool)
          ]
        })
      },
      |env: PgSqlEnv, _kind| {
        let pool = env.1.0.0.clone();
        close_pool(pool)
      },
    )
  }
}

async fn ping(pool: &PgPool) -> Result<(), SqlError> {
  let mut conn = pool.acquire().await.map_err(sqlx_error)?;
  sqlx::query("SELECT 1")
    .execute(&mut *conn)
    .await
    .map_err(sqlx_error)?;
  Ok(())
}

fn close_pool(pool: PgPool) -> Effect<(), Never> {
  from_async(move || async move {
    pool.close().await;
    Ok(())
  })
}

impl SqlClient for PgSqlClient {
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
        .map_err(sqlx_error)?;
      decode_rows(&rows)
    })
  }

  fn execute(&self, sql: &str, params: &[SqlValue]) -> Effect<u64, SqlError> {
    let (pool, sql, params) = (self.pool.clone(), sql.to_string(), params.to_vec());
    from_async(move || async move {
      let result = bind_query(sqlx::query(&sql), &params)?
        .execute(&pool)
        .await
        .map_err(sqlx_error)?;
      Ok(result.rows_affected())
    })
  }

  fn begin(&self) -> Effect<Arc<dyn SqlTransaction>, SqlError> {
    let pool = self.pool.clone();
    from_async(move || async move {
      let tx = pool.begin().await.map_err(sqlx_error)?;
      Ok(Arc::new(PgSqlTransaction::new(tx)) as Arc<dyn SqlTransaction>)
    })
  }

  /// Streams the rows from a sqlx `fetch` on a pooled connection, buffering at most two batches
  /// of 64 rows; dropping or interrupting the stream cancels the query on the server.
  fn query_stream(&self, sql: &str, params: &[SqlValue]) -> id_effect::Stream<SqlRow, SqlError> {
    super::stream::query_stream(self.pool.clone(), sql.to_owned(), params.to_vec())
  }

  fn close(&self) -> Effect<(), Never> {
    close_pool(self.pool.clone())
  }
}
