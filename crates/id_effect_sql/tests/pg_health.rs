//! `pg` feature: the pool health check ([`pg_pool_health`]).
//!
//! The live test needs a database at `DATABASE_URL`; like `tests/pg.rs` it fails when
//! `ID_EFFECT_REQUIRE_PG=1` and the database is missing, and otherwise prints a `SKIP` line and
//! returns.
#![cfg(feature = "pg")]

use id_effect::{Cause, Effect, Exit};
use id_effect_sql::SqlError;
use id_effect_sql::health::pg_pool_health;
use id_effect_sql::pg::{PgPoolConfig, pg_pool_from_config, pg_pool_from_config_lazy};
use id_effect_tokio::TokioRuntime;

async fn run<A: Send + 'static>(eff: Effect<A, SqlError>) -> Result<A, SqlError> {
  match TokioRuntime::current().expect("tokio").run(eff).await {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
  }
}

fn skip(test: &str, why: &str) {
  assert!(
    !std::env::var("ID_EFFECT_REQUIRE_PG").is_ok_and(|v| v == "1"),
    "{test}: ID_EFFECT_REQUIRE_PG=1 but Postgres is unavailable: {why}"
  );
  eprintln!("SKIP {test}: {why} (set DATABASE_URL, and ID_EFFECT_REQUIRE_PG=1 to require it)");
}

#[tokio::test]
async fn pg_pool_health_is_down_when_the_server_is_unreachable() {
  let pool = pg_pool_from_config_lazy(
    PgPoolConfig::from_url("postgres://127.0.0.1:1/postgres").with_max_size(1),
  )
  .expect("pool");
  assert!(run(pg_pool_health(pool)).await.is_err());
}

#[tokio::test]
async fn live_postgres_pool_health_reports_size_and_idle() {
  let test = "live_postgres_pool_health_reports_size_and_idle";
  let Ok(url) = std::env::var("DATABASE_URL") else {
    return skip(test, "DATABASE_URL is not set");
  };
  let pool = match pg_pool_from_config(PgPoolConfig::from_url(url).with_max_size(3)).await {
    Ok(p) => p,
    Err(e) => return skip(test, &format!("cannot build pool: {}", e.0)),
  };
  let detail = run(pg_pool_health(pool)).await.expect("pool health");
  assert_eq!(detail["max_size"], 3);
  assert!(detail["size"] >= 1, "{detail:?}");
  assert!(detail["idle"] <= detail["size"], "{detail:?}");
}
