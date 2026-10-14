//! Unit and integration tests for the `pg` feature (formerly `id_effect_sql_pg`).
//!
//! `live_postgres_*` tests need a database at `DATABASE_URL`. When CI provides a Postgres service
//! it also sets `ID_EFFECT_REQUIRE_PG=1`, which turns a missing or unreachable database into a test
//! **failure**. Without it, those tests print an explicit `SKIP` line to stderr and return — they
//! never silently pass with a database that is present but misbehaving.

use id_effect::run_async;
use id_effect_sql::pg::{
  PgPool, PgPoolConfig, PgSqlClient, pg_pool_from_config, pg_pool_from_config_lazy,
  provide_pg_sql_client,
};
use id_effect_sql::{SqlClient, SqlParam};

fn lazy_pool() -> sqlx::PgPool {
  pg_pool_from_config_lazy(PgPoolConfig::from_url("postgres://localhost:5432/postgres"))
    .expect("pool")
}

fn pg_required() -> bool {
  std::env::var("ID_EFFECT_REQUIRE_PG").is_ok_and(|v| v == "1")
}

/// Fail (when `ID_EFFECT_REQUIRE_PG=1`) or loudly skip a live test that has no database.
fn unavailable(test: &str, why: &str) -> Option<PgSqlClient> {
  assert!(
    !pg_required(),
    "{test}: ID_EFFECT_REQUIRE_PG=1 but Postgres is unavailable: {why}"
  );
  eprintln!("SKIP {test}: {why} (set DATABASE_URL, and ID_EFFECT_REQUIRE_PG=1 to require it)");
  None
}

/// Connected client for a live test, or `None` after [`unavailable`] decided to skip.
async fn live_client(test: &str) -> Option<PgSqlClient> {
  let Ok(url) = std::env::var("DATABASE_URL") else {
    return unavailable(test, "DATABASE_URL is not set");
  };
  let pool = match pg_pool_from_config(PgPoolConfig::from_url(url).with_max_size(2)).await {
    Ok(p) => p,
    Err(e) => return unavailable(test, &format!("cannot build pool: {}", e.0)),
  };
  let client = PgSqlClient::new(pool);
  if let Err(e) = run_async(client.connect(), ()).await {
    return unavailable(test, &format!("cannot connect: {e:?}"));
  }
  Some(client)
}

#[tokio::test]
async fn invalid_postgres_url_is_rejected_at_connect() {
  let err = pg_pool_from_config(PgPoolConfig::from_url("not-a-url"))
    .await
    .unwrap_err();
  let msg = err.0.to_string();
  assert!(!msg.is_empty());
}

#[test]
fn pg_pool_config_with_max_size() {
  let cfg = PgPoolConfig::from_url("postgres://localhost/db").with_max_size(4);
  assert_eq!(cfg.max_size, 4);
}

#[tokio::test]
async fn valid_url_builds_pool_without_connecting() {
  let pool = lazy_pool();
  assert!(pool.options().get_max_connections() > 0);
}

#[tokio::test]
async fn provide_pg_sql_client_builds_provider() {
  let _provider = provide_pg_sql_client(lazy_pool());
}

#[tokio::test]
async fn connect_fails_when_server_unreachable() {
  let pool = pg_pool_from_config_lazy(
    PgPoolConfig::from_url("postgres://127.0.0.1:1/postgres").with_max_size(1),
  )
  .expect("pool");
  let client = PgSqlClient::new(pool);
  assert!(run_async(client.connect(), ()).await.is_err());
}

#[tokio::test]
async fn query_fails_when_server_unreachable() {
  let pool = pg_pool_from_config_lazy(
    PgPoolConfig::from_url("postgres://127.0.0.1:1/postgres").with_max_size(1),
  )
  .expect("pool");
  let client = PgSqlClient::new(pool);
  assert!(run_async(client.query("SELECT 1", &[]), ()).await.is_err());
}

#[tokio::test]
async fn execute_fails_when_server_unreachable() {
  let pool = pg_pool_from_config_lazy(
    PgPoolConfig::from_url("postgres://127.0.0.1:1/postgres").with_max_size(1),
  )
  .expect("pool");
  let client = PgSqlClient::new(pool);
  assert!(
    run_async(client.execute("SELECT 1", &[]), ())
      .await
      .is_err()
  );
}

#[tokio::test]
async fn begin_fails_when_server_unreachable() {
  let pool = pg_pool_from_config_lazy(
    PgPoolConfig::from_url("postgres://127.0.0.1:1/postgres").with_max_size(1),
  )
  .expect("pool");
  let client = PgSqlClient::new(pool);
  assert!(run_async(client.begin(), ()).await.is_err());
}

#[test]
fn provider_builds_env_with_sql_client() {
  use id_effect::{Cap, build_env};
  use id_effect_sql::SqlClientService;
  let rt = tokio::runtime::Builder::new_current_thread()
    .enable_all()
    .build()
    .expect("rt");
  let pool = rt.block_on(async { lazy_pool() });
  let env = build_env([provide_pg_sql_client(pool)]).expect("env");
  assert!(env.has::<Cap<SqlClientService>>());
  assert!(env.has::<Cap<PgPool>>());
}

#[tokio::test]
async fn live_postgres_round_trip() {
  let Some(client) = live_client("live_postgres_round_trip").await else {
    return;
  };
  let rows = run_async(
    client.query("SELECT $1::int AS n", &[SqlParam::Int(42)]),
    (),
  )
  .await
  .expect("query");
  assert_eq!(rows[0].cells.first().map(String::as_str), Some("42"));
  run_async(client.execute("SELECT 1", &[]), ())
    .await
    .expect("execute");
  let tx = run_async(client.begin(), ()).await.expect("begin");
  run_async(tx.commit(), ()).await.expect("commit");
}

#[tokio::test]
async fn live_postgres_transaction_commit_and_rollback() {
  let Some(client) = live_client("live_postgres_transaction_commit_and_rollback").await else {
    return;
  };
  let tx = run_async(client.begin(), ()).await.expect("begin");
  run_async(tx.commit(), ()).await.expect("commit");
  let tx2 = run_async(client.begin(), ()).await.expect("begin 2");
  run_async(tx2.rollback(), ()).await.expect("rollback");
}

#[tokio::test]
async fn live_postgres_all_param_types_and_row_decode() {
  let Some(client) = live_client("live_postgres_all_param_types_and_row_decode").await else {
    return;
  };
  let rows = run_async(
    client.query(
      "SELECT $1::bool AS b, $2::text AS t, $3::bytea AS bytes, $4::int AS n, NULL::int AS nil, 3.14::float AS f",
      &[
        SqlParam::Bool(true),
        SqlParam::Text("hello".into()),
        SqlParam::Bytes(vec![1, 2, 3]),
        SqlParam::Int(99),
      ],
    ),
    (),
  )
  .await
  .expect("typed query");
  assert_eq!(rows[0].cells.get(1).map(String::as_str), Some("hello"));
  assert_eq!(rows[0].cells.get(3).map(String::as_str), Some("99"));
  assert_eq!(rows[0].cells.get(4).map(String::as_str), Some(""));
  assert!(rows[0].cells.get(5).is_some_and(|c| c.starts_with("3.14")));
  let null_rows = run_async(client.query("SELECT $1::int", &[SqlParam::Null]), ())
    .await
    .expect("null param query");
  assert_eq!(null_rows.len(), 1);
  assert!(
    run_async(client.execute("SELECT FROM", &[]), ())
      .await
      .is_err(),
    "malformed SQL must fail"
  );
}

#[tokio::test]
async fn live_postgres_double_finish_fails() {
  let Some(client) = live_client("live_postgres_double_finish_fails").await else {
    return;
  };
  let tx = run_async(client.begin(), ()).await.expect("begin");
  run_async(tx.commit(), ()).await.expect("first commit");
  assert!(run_async(tx.commit(), ()).await.is_err());
  assert!(run_async(tx.rollback(), ()).await.is_err());
}
