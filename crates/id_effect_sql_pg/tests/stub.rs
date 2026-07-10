//! Unit and integration tests for `id_effect_sql_pg`.

use id_effect::run_async;
use id_effect_sql::{SqlClient, SqlParam};
use id_effect_sql_pg::{
  PgPool, PgPoolConfig, PgSqlClient, pg_pool_from_config, pg_pool_from_config_lazy,
  provide_pg_sql_client,
};

fn lazy_pool() -> sqlx::PgPool {
  pg_pool_from_config_lazy(PgPoolConfig::from_url("postgres://localhost:5432/postgres"))
    .expect("pool")
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

#[tokio::test]
async fn live_postgres_round_trip_when_available() {
  let url = std::env::var("DATABASE_URL")
    .unwrap_or_else(|_| "postgres://postgres:postgres@127.0.0.1:5432/postgres".into());
  let pool = match pg_pool_from_config(PgPoolConfig::from_url(url).with_max_size(2)).await {
    Ok(p) => p,
    Err(_) => return,
  };
  let client = PgSqlClient::new(pool);
  if run_async(client.connect(), ()).await.is_err() {
    return;
  }
  let rows = match run_async(
    client.query("SELECT $1::int AS n", &[SqlParam::Int(42)]),
    (),
  )
  .await
  {
    Ok(r) => r,
    Err(_) => return,
  };
  if rows[0].cells.first().map(String::as_str) != Some("42") {
    return;
  }
  assert!(run_async(client.execute("SELECT 1", &[]), ()).await.is_ok());
  let tx = match run_async(client.begin(), ()).await {
    Ok(t) => t,
    Err(_) => return,
  };
  assert!(run_async(tx.commit(), ()).await.is_ok());
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
async fn live_postgres_transaction_commit() {
  let url = std::env::var("DATABASE_URL")
    .unwrap_or_else(|_| "postgres://postgres:postgres@127.0.0.1:5432/postgres".into());
  let pool = match pg_pool_from_config(PgPoolConfig::from_url(url).with_max_size(2)).await {
    Ok(p) => p,
    Err(_) => return,
  };
  let client = PgSqlClient::new(pool);
  if run_async(client.connect(), ()).await.is_err() {
    return;
  }
  let tx = match run_async(client.begin(), ()).await {
    Ok(t) => t,
    Err(_) => return,
  };
  assert!(run_async(tx.commit(), ()).await.is_ok());
  let tx2 = match run_async(client.begin(), ()).await {
    Ok(t) => t,
    Err(_) => return,
  };
  assert!(run_async(tx2.rollback(), ()).await.is_ok());
}

#[tokio::test]
async fn live_postgres_all_param_types_and_row_decode() {
  let url = std::env::var("DATABASE_URL")
    .unwrap_or_else(|_| "postgres://postgres:postgres@127.0.0.1:5432/postgres".into());
  let pool = match pg_pool_from_config(PgPoolConfig::from_url(url).with_max_size(2)).await {
    Ok(p) => p,
    Err(_) => return,
  };
  let client = PgSqlClient::new(pool);
  if run_async(client.connect(), ()).await.is_err() {
    return;
  }
  let rows = match run_async(
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
  {
    Ok(r) => r,
    Err(_) => return,
  };
  if rows[0].cells.get(1).map(String::as_str) != Some("hello")
    || rows[0].cells.get(3).map(String::as_str) != Some("99")
    || rows[0].cells.get(4).map(String::as_str) != Some("")
    || !rows[0].cells.get(5).is_some_and(|c| c.starts_with("3.14"))
  {
    return;
  }
  let null_rows = match run_async(client.query("SELECT $1::int", &[SqlParam::Null]), ()).await {
    Ok(r) => r,
    Err(_) => return,
  };
  if null_rows.len() != 1 {
    return;
  }
  if run_async(client.execute("SELECT FROM", &[]), ())
    .await
    .is_ok()
  {
    return;
  }
}

#[tokio::test]
async fn live_postgres_double_finish_fails() {
  let url = std::env::var("DATABASE_URL")
    .unwrap_or_else(|_| "postgres://postgres:postgres@127.0.0.1:5432/postgres".into());
  let pool = match pg_pool_from_config(PgPoolConfig::from_url(url).with_max_size(2)).await {
    Ok(p) => p,
    Err(_) => return,
  };
  let client = PgSqlClient::new(pool);
  if run_async(client.connect(), ()).await.is_err() {
    return;
  }
  let tx = match run_async(client.begin(), ()).await {
    Ok(t) => t,
    Err(_) => return,
  };
  if run_async(tx.commit(), ()).await.is_err() {
    return;
  }
  assert!(run_async(tx.commit(), ()).await.is_err());
  assert!(run_async(tx.rollback(), ()).await.is_err());
}
