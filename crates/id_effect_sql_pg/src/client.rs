//! [`PgSqlClient`] — [`SqlClient`](id_effect_sql::SqlClient) over sqlx [`PgPool`].

use std::sync::Arc;

use id_effect::kernel::Effect;
use id_effect_sql::{SqlClient, SqlError, SqlParam, SqlRow, SqlTransaction};
use sqlx::{AssertSqlSafe, PgPool, Row as _};

use crate::error::sqlx_error;
use crate::transaction::PgSqlTransaction;

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
}

fn bind_query<'q>(
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

fn row_to_sql_row(row: &sqlx::postgres::PgRow) -> SqlRow {
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
  fn connect(&self) -> Effect<(), SqlError, ()> {
    let pool = self.pool.clone();
    Effect::new_async(move |_r: &mut ()| {
      Box::pin(async move {
        let mut conn = pool.acquire().await.map_err(sqlx_error)?;
        sqlx::query("SELECT 1")
          .execute(&mut *conn)
          .await
          .map_err(sqlx_error)?;
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
        let query = bind_query(sqlx::query(AssertSqlSafe(sql)), &params);
        let rows = query.fetch_all(&pool).await.map_err(sqlx_error)?;
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
        let query = bind_query(sqlx::query(AssertSqlSafe(sql)), &params);
        let result = query.execute(&pool).await.map_err(sqlx_error)?;
        Ok(result.rows_affected())
      })
    })
  }

  fn begin(&self) -> Effect<Arc<dyn SqlTransaction>, SqlError, ()> {
    let pool = self.pool.clone();
    Effect::new_async(move |_r: &mut ()| {
      Box::pin(async move {
        let mut conn = pool.acquire().await.map_err(sqlx_error)?;
        sqlx::query("BEGIN")
          .execute(&mut *conn)
          .await
          .map_err(sqlx_error)?;
        Ok(Arc::new(PgSqlTransaction::new(conn)) as Arc<dyn SqlTransaction>)
      })
    })
  }
}
