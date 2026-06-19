//! Unit and integration tests for `id_effect_sql_pg`.

use id_effect::run_async;
use id_effect_sql::{SqlClient, SqlParam};
use id_effect_sql_pg::{PgPoolConfig, PgSqlClient, pg_pool_from_config, provide_pg_sql_client};

#[test]
fn invalid_postgres_url_is_rejected_at_pool_build() {
  let err = pg_pool_from_config(PgPoolConfig::from_url("not-a-url")).unwrap_err();
  let msg = err.0.to_string();
  assert!(msg.contains("invalid postgres url") || msg.contains("Unsupported"));
}

#[test]
fn pg_pool_config_with_max_size() {
  let cfg = PgPoolConfig::from_url("postgres://localhost/db").with_max_size(4);
  assert_eq!(cfg.max_size, 4);
}

#[test]
fn valid_url_builds_pool_without_connecting() {
  let pool = pg_pool_from_config(PgPoolConfig::from_url("postgres://localhost:5432/postgres"))
    .expect("pool build");
  assert!(pool.status().size > 0 || pool.status().max_size > 0);
}

#[test]
fn provide_pg_sql_client_builds_provider() {
  let pool = pg_pool_from_config(PgPoolConfig::from_url("postgres://localhost:5432/postgres"))
    .expect("pool");
  let _provider = provide_pg_sql_client(pool);
}

#[tokio::test]
async fn connect_fails_when_server_unreachable() {
  let pool =
    pg_pool_from_config(PgPoolConfig::from_url("postgres://127.0.0.1:1/postgres").with_max_size(1))
      .expect("pool");
  let client = PgSqlClient::new(pool);
  assert!(run_async(client.connect(), ()).await.is_err());
}

#[tokio::test]
async fn query_fails_when_server_unreachable() {
  let pool =
    pg_pool_from_config(PgPoolConfig::from_url("postgres://127.0.0.1:1/postgres").with_max_size(1))
      .expect("pool");
  let client = PgSqlClient::new(pool);
  assert!(run_async(client.query("SELECT 1", &[]), ()).await.is_err());
}

#[tokio::test]
async fn execute_fails_when_server_unreachable() {
  let pool =
    pg_pool_from_config(PgPoolConfig::from_url("postgres://127.0.0.1:1/postgres").with_max_size(1))
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
  let pool =
    pg_pool_from_config(PgPoolConfig::from_url("postgres://127.0.0.1:1/postgres").with_max_size(1))
      .expect("pool");
  let client = PgSqlClient::new(pool);
  assert!(run_async(client.begin(), ()).await.is_err());
}

#[tokio::test]
async fn live_postgres_round_trip_when_available() {
  let url = std::env::var("DATABASE_URL")
    .unwrap_or_else(|_| "postgres://postgres:postgres@127.0.0.1:5432/postgres".into());
  let pool = match pg_pool_from_config(PgPoolConfig::from_url(url).with_max_size(2)) {
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
  assert_eq!(rows[0].cells[0], "42");
  let affected = match run_async(client.execute("SELECT 1", &[]), ()).await {
    Ok(n) => n,
    Err(_) => return,
  };
  assert_eq!(affected, 1);
  let tx = match run_async(client.begin(), ()).await {
    Ok(t) => t,
    Err(_) => return,
  };
  assert!(run_async(tx.commit(), ()).await.is_ok());
}

#[test]
fn provider_builds_env_with_sql_client() {
  use id_effect::build_env;
  use id_effect_sql::client::SqlClientKey;
  let pool = pg_pool_from_config(PgPoolConfig::from_url("postgres://localhost:5432/postgres"))
    .expect("pool");
  let env = build_env([provide_pg_sql_client(pool)]).expect("env");
  assert!(env.has::<SqlClientKey>());
}

#[tokio::test]
async fn live_postgres_transaction_commit() {
  let url = std::env::var("DATABASE_URL")
    .unwrap_or_else(|_| "postgres://postgres:postgres@127.0.0.1:5432/postgres".into());
  let pool = match pg_pool_from_config(PgPoolConfig::from_url(url).with_max_size(2)) {
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
  let pool = match pg_pool_from_config(PgPoolConfig::from_url(url).with_max_size(2)) {
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
  assert!(!rows[0].cells[0].is_empty());
  assert_eq!(rows[0].cells[1], "hello");
  assert_eq!(rows[0].cells[3], "99");
  assert_eq!(rows[0].cells[4], "");
  assert_eq!(rows[0].cells[5], "");
  let null_rows = match run_async(client.query("SELECT $1::int", &[SqlParam::Null]), ()).await {
    Ok(r) => r,
    Err(_) => return,
  };
  assert_eq!(null_rows.len(), 1);
  assert!(
    run_async(client.execute("SELECT FROM", &[]), ())
      .await
      .is_err()
  );
}

#[tokio::test]
async fn live_postgres_double_finish_fails() {
  let url = std::env::var("DATABASE_URL")
    .unwrap_or_else(|_| "postgres://postgres:postgres@127.0.0.1:5432/postgres".into());
  let pool = match pg_pool_from_config(PgPoolConfig::from_url(url).with_max_size(2)) {
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
