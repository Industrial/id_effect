//! [`PgSqlClient`] — [`SqlClient`](crate::SqlClient) over sqlx [`PgPool`].

use std::sync::Arc;

use id_effect::Never;
use id_effect::next::{Effect, Layer, from_async, hlist};
use sqlx::{PgPool, Row as _};

use super::config::{PgPoolConfig, pg_pool_from_config};
use super::error::sqlx_error;
use super::pool_key::PgPool as PgPoolCap;
use super::providers::PgSqlEnv;
use super::transaction::PgSqlTransaction;
use crate::{SqlClient, SqlClientService, SqlError, SqlParam, SqlRow, SqlTransaction};

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

pub(super) fn bind_query<'q>(
  mut query: sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>,
  params: &'q [SqlParam],
) -> sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments> {
  for param in params {
    query = match param {
      SqlParam::Null => query.bind(None::<i32>),
      SqlParam::Bool(v) => query.bind(*v),
      SqlParam::Int(v) => query.bind(*v),
      SqlParam::Text(v) => query.bind(v.clone()),
      SqlParam::Bytes(v) => query.bind(v.clone()),
    };
  }
  query
}

pub(super) fn row_to_sql_row(row: &sqlx::postgres::PgRow) -> SqlRow {
  let cells = (0..row.len())
    .map(|idx| {
      if let Ok(v) = row.try_get::<Option<i32>, _>(idx) {
        return v.map(|n| n.to_string()).unwrap_or_default();
      }
      if let Ok(v) = row.try_get::<Option<i64>, _>(idx) {
        return v.map(|n| n.to_string()).unwrap_or_default();
      }
      if let Ok(v) = row.try_get::<Option<bool>, _>(idx) {
        return v.map(|b| b.to_string()).unwrap_or_default();
      }
      if let Ok(v) = row.try_get::<Option<f64>, _>(idx) {
        return v.map(|f| f.to_string()).unwrap_or_default();
      }
      if let Ok(v) = row.try_get::<Option<String>, _>(idx) {
        return v.unwrap_or_default();
      }
      if let Ok(v) = row.try_get::<Option<Vec<u8>>, _>(idx) {
        return v.map(|b| format!("{b:?}")).unwrap_or_default();
      }
      String::new()
    })
    .collect();
  SqlRow::new(cells)
}

impl SqlClient for PgSqlClient {
  fn connect(&self) -> Effect<(), SqlError> {
    let pool = self.pool.clone();
    from_async(move || async move { ping(&pool).await })
  }

  fn query(&self, sql: &str, params: &[SqlParam]) -> Effect<Vec<SqlRow>, SqlError> {
    let (pool, sql, params) = (self.pool.clone(), sql.to_string(), params.to_vec());
    from_async(move || async move {
      let rows = bind_query(sqlx::query(&sql), &params)
        .fetch_all(&pool)
        .await
        .map_err(sqlx_error)?;
      Ok(rows.iter().map(row_to_sql_row).collect())
    })
  }

  fn execute(&self, sql: &str, params: &[SqlParam]) -> Effect<u64, SqlError> {
    let (pool, sql, params) = (self.pool.clone(), sql.to_string(), params.to_vec());
    from_async(move || async move {
      let result = bind_query(sqlx::query(&sql), &params)
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

  fn close(&self) -> Effect<(), Never> {
    close_pool(self.pool.clone())
  }
}
