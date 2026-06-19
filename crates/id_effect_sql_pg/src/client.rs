//! [`PgSqlClient`] — [`SqlClient`](id_effect_sql::SqlClient) over deadpool-postgres.

use std::sync::Arc;

use deadpool_postgres::Pool;
use id_effect::kernel::Effect;
use id_effect_sql::{SqlClient, SqlError, SqlParam, SqlRow, SqlTransaction};
use tokio_postgres::types::ToSql;

use crate::error::{pg_error, pool_error};
use crate::transaction::PgSqlTransaction;

/// PostgreSQL [`SqlClient`] backed by a deadpool connection pool.
#[derive(Clone)]
pub struct PgSqlClient {
  pool: Pool,
}

impl PgSqlClient {
  /// Wrap an existing deadpool [`Pool`].
  #[inline]
  pub fn new(pool: Pool) -> Self {
    Self { pool }
  }

  /// Borrow the underlying pool (for advanced wiring).
  #[inline]
  pub fn pool(&self) -> &Pool {
    &self.pool
  }
}

fn bind_params(params: &[SqlParam]) -> Vec<Box<dyn ToSql + Sync + Send>> {
  params
    .iter()
    .map(|p| match p {
      SqlParam::Null => Box::new(None::<i32>) as Box<dyn ToSql + Sync + Send>,
      SqlParam::Bool(v) => Box::new(*v),
      SqlParam::Int(v) => Box::new(*v),
      SqlParam::Text(v) => Box::new(v.clone()),
      SqlParam::Bytes(v) => Box::new(v.clone()),
    })
    .collect()
}

fn row_to_sql_row(row: &tokio_postgres::Row) -> SqlRow {
  let cells = (0..row.len())
    .map(|idx| {
      if let Ok(v) = row.try_get::<_, Option<String>>(idx) {
        return v.unwrap_or_default();
      }
      if let Ok(v) = row.try_get::<_, Option<i64>>(idx) {
        return v.map(|n| n.to_string()).unwrap_or_default();
      }
      if let Ok(v) = row.try_get::<_, Option<bool>>(idx) {
        return v.map(|b| b.to_string()).unwrap_or_default();
      }
      String::new()
    })
    .collect();
  SqlRow::new(cells)
}

impl SqlClient for PgSqlClient {
  fn connect(&self) -> Effect<(), SqlError, ()> {
    let pool = self.pool.clone();
    Effect::new_async(move |_r: &mut ()| {
      Box::pin(async move {
        let client = pool.get().await.map_err(pool_error)?;
        drop(client);
        Ok(())
      })
    })
  }

  fn query(&self, sql: &str, params: &[SqlParam]) -> Effect<Vec<SqlRow>, SqlError, ()> {
    let pool = self.pool.clone();
    let sql = sql.to_string();
    let params = params.to_vec();
    Effect::new_async(move |_r: &mut ()| {
      Box::pin(async move {
        let client = pool.get().await.map_err(pool_error)?;
        let binds = bind_params(&params);
        let refs: Vec<&(dyn ToSql + Sync)> = binds
          .iter()
          .map(|b| b.as_ref() as &(dyn ToSql + Sync))
          .collect();
        let rows = client.query(&sql, &refs).await.map_err(pg_error)?;
        Ok(rows.iter().map(row_to_sql_row).collect())
      })
    })
  }

  fn execute(&self, sql: &str, params: &[SqlParam]) -> Effect<u64, SqlError, ()> {
    let pool = self.pool.clone();
    let sql = sql.to_string();
    let params = params.to_vec();
    Effect::new_async(move |_r: &mut ()| {
      Box::pin(async move {
        let client = pool.get().await.map_err(pool_error)?;
        let binds = bind_params(&params);
        let refs: Vec<&(dyn ToSql + Sync)> = binds
          .iter()
          .map(|b| b.as_ref() as &(dyn ToSql + Sync))
          .collect();
        let n = client.execute(&sql, &refs).await.map_err(pg_error)?;
        Ok(n)
      })
    })
  }

  fn begin(&self) -> Effect<Arc<dyn SqlTransaction>, SqlError, ()> {
    let pool = self.pool.clone();
    Effect::new_async(move |_r: &mut ()| {
      Box::pin(async move {
        let client = pool.get().await.map_err(pool_error)?;
        client.execute("BEGIN", &[]).await.map_err(pg_error)?;
        Ok(Arc::new(PgSqlTransaction::new(client)) as Arc<dyn SqlTransaction>)
      })
    })
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::{PgPoolConfig, pg_pool_from_config};

  #[test]
  fn pool_config_rejects_invalid_url() {
    let err = pg_pool_from_config(PgPoolConfig::from_url("not-a-url")).unwrap_err();
    assert!(matches!(err.0, SqlError::Unsupported(_)));
  }
  #[test]
  fn pg_sql_client_exposes_pool() {
    let pool = pg_pool_from_config(PgPoolConfig::from_url("postgres://localhost:5432/postgres"))
      .expect("pool");
    let client = PgSqlClient::new(pool);
    assert!(client.pool().status().max_size > 0);
  }
}
