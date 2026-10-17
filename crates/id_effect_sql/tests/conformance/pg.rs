//! The conformance suite on PostgreSQL at `DATABASE_URL`.
//!
//! Without a database the tests print `SKIP` and return, unless `ID_EFFECT_REQUIRE_PG=1` (CI),
//! which turns a missing or unreachable database into a failure.

use std::sync::Arc;

use id_effect_sql::pg::{PgPoolConfig, PgSqlClient, pg_pool_from_config};
use id_effect_sql::{SqlClient, SqlType};

use crate::conformance::{Backend, conformance_suite, run};

pub struct Pg(PgSqlClient);

impl Backend for Pg {
  fn client(&self) -> Arc<dyn SqlClient> {
    Arc::new(self.0.clone())
  }

  fn column_ddl(&self, t: SqlType) -> &'static str {
    match t {
      SqlType::Bool => "BOOLEAN",
      SqlType::I16 => "SMALLINT",
      SqlType::I32 => "INTEGER",
      SqlType::I64 => "BIGINT",
      SqlType::F32 => "REAL",
      SqlType::F64 => "DOUBLE PRECISION",
      SqlType::Bytes => "BYTEA",
      SqlType::Uuid => "UUID",
      SqlType::Timestamp => "TIMESTAMPTZ",
      SqlType::DateTime => "TIMESTAMP",
      SqlType::Date => "DATE",
      SqlType::Time => "TIME",
      SqlType::Decimal => "NUMERIC",
      SqlType::Json => "JSONB",
      _ => "TEXT",
    }
  }

  fn placeholder(&self, n: usize) -> String {
    format!("${n}")
  }
}

fn unavailable(test: &str, why: &str) -> Option<Pg> {
  assert!(
    !std::env::var("ID_EFFECT_REQUIRE_PG").is_ok_and(|v| v == "1"),
    "{test}: ID_EFFECT_REQUIRE_PG=1 but Postgres is unavailable: {why}"
  );
  eprintln!("SKIP {test}: {why} (set DATABASE_URL, and ID_EFFECT_REQUIRE_PG=1 to require it)");
  None
}

/// A connected client with a pool of [`Backend::max_connections`], or `None` (skip).
pub async fn live_pg(test: &str) -> Option<Pg> {
  let Ok(url) = std::env::var("DATABASE_URL") else {
    return unavailable(test, "DATABASE_URL is not set");
  };
  let pool = match pg_pool_from_config(PgPoolConfig::from_url(url).with_max_size(4)).await {
    Ok(pool) => pool,
    Err(e) => return unavailable(test, &format!("cannot build pool: {}", e.0)),
  };
  let client = PgSqlClient::new(pool);
  if let Err(e) = run(client.connect()).await {
    return unavailable(test, &format!("cannot connect: {e:?}"));
  }
  Some(Pg(client))
}

conformance_suite!("cf_pg", live_pg);
