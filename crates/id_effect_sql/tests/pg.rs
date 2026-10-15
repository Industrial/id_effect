//! Unit and integration tests for the `pg` feature.
//!
//! `live_postgres_*` tests need a database at `DATABASE_URL`. When CI provides a Postgres service
//! it also sets `ID_EFFECT_REQUIRE_PG=1`, which turns a missing or unreachable database into a test
//! **failure**. Without it, those tests print an explicit `SKIP` line to stderr and return — they
//! never silently pass with a database that is present but misbehaving.
//!
//! Effects run on `id_effect_tokio::TokioRuntime` (sqlx needs tokio's IO driver).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use id_effect::next::{Effect, HCons, HNil, fail, need, sleep, sync};
use id_effect::{Cause, Exit};
use id_effect_sql::pg::{
  PgPool, PgPoolConfig, PgSqlClient, PgSqlEnv, pg_pool_from_config, pg_pool_from_config_lazy,
  provide_pg_pool, provide_pg_sql_client,
};
use id_effect_sql::{
  SqlClient, SqlClientService, SqlError, SqlParam, SqlTransactionEnv, SqlTransactionService, query,
  transaction_layer, with_transaction,
};
use id_effect_tokio::TokioRuntime;

fn lazy_pool() -> sqlx::PgPool {
  pg_pool_from_config_lazy(PgPoolConfig::from_url("postgres://localhost:5432/postgres"))
    .expect("pool")
}

fn unreachable_client() -> PgSqlClient {
  let pool = pg_pool_from_config_lazy(
    PgPoolConfig::from_url("postgres://127.0.0.1:1/postgres").with_max_size(1),
  )
  .expect("pool");
  PgSqlClient::new(pool)
}

async fn exit<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> Exit<A, E> {
  TokioRuntime::current().expect("tokio").run(eff).await
}

async fn run<A: Send + 'static>(eff: Effect<A, SqlError>) -> Result<A, SqlError> {
  match exit(eff).await {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
  }
}

fn pg_required() -> bool {
  std::env::var("ID_EFFECT_REQUIRE_PG").is_ok_and(|v| v == "1")
}

/// Fail (when `ID_EFFECT_REQUIRE_PG=1`) or loudly skip a live test that has no database.
fn unavailable<T>(test: &str, why: &str) -> Option<T> {
  assert!(
    !pg_required(),
    "{test}: ID_EFFECT_REQUIRE_PG=1 but Postgres is unavailable: {why}"
  );
  eprintln!("SKIP {test}: {why} (set DATABASE_URL, and ID_EFFECT_REQUIRE_PG=1 to require it)");
  None
}

fn live_config(test: &str) -> Option<PgPoolConfig> {
  match std::env::var("DATABASE_URL") {
    Ok(url) => Some(PgPoolConfig::from_url(url).with_max_size(4)),
    Err(_) => unavailable(test, "DATABASE_URL is not set"),
  }
}

/// Connected client for a live test, or `None` after [`unavailable`] decided to skip.
async fn live_client(test: &str) -> Option<PgSqlClient> {
  let config = live_config(test)?;
  let pool = match pg_pool_from_config(config).await {
    Ok(p) => p,
    Err(e) => return unavailable(test, &format!("cannot build pool: {}", e.0)),
  };
  let client = PgSqlClient::new(pool);
  if let Err(e) = run(client.connect()).await {
    return unavailable(test, &format!("cannot connect: {e:?}"));
  }
  Some(client)
}

/// A fresh table `name (n int)` for one test.
async fn table(client: &PgSqlClient, name: &str) {
  run(client.execute(&format!("DROP TABLE IF EXISTS {name}"), &[]))
    .await
    .expect("drop");
  run(client.execute(&format!("CREATE TABLE {name} (n int)"), &[]))
    .await
    .expect("create");
}

async fn count(client: &PgSqlClient, name: &str) -> String {
  let rows = run(client.query(&format!("SELECT COUNT(*)::int FROM {name}"), &[]))
    .await
    .expect("count");
  rows[0].cells[0].clone()
}

async fn drop_table(client: &PgSqlClient, name: &str) {
  run(client.execute(&format!("DROP TABLE IF EXISTS {name}"), &[]))
    .await
    .expect("drop");
}

#[tokio::test]
async fn invalid_postgres_url_is_rejected_at_connect() {
  let err = pg_pool_from_config(PgPoolConfig::from_url("not-a-url"))
    .await
    .unwrap_err();
  assert!(!err.0.to_string().is_empty());
}

#[test]
fn pg_pool_config_with_max_size() {
  let cfg = PgPoolConfig::from_url("postgres://localhost/db").with_max_size(4);
  assert_eq!(cfg.max_size, 4);
}

#[tokio::test]
async fn valid_url_builds_pool_without_connecting() {
  assert!(lazy_pool().options().get_max_connections() > 0);
}

#[tokio::test]
async fn statements_fail_when_server_unreachable() {
  let client = unreachable_client();
  assert!(run(client.connect()).await.is_err());
  assert!(run(client.query("SELECT 1", &[])).await.is_err());
  assert!(run(client.execute("SELECT 1", &[])).await.is_err());
  assert!(run(client.begin()).await.is_err());
}

#[tokio::test]
async fn provide_pg_sql_client_provides_the_client_and_the_pool() {
  let body: Effect<bool, SqlError, PgSqlEnv> =
    need::<SqlClientService>().flat_map(|_| need::<PgPool>().map(|p: Arc<PgPool>| !p.is_closed()));
  assert!(
    run(body.provide_layer(provide_pg_sql_client(lazy_pool())))
      .await
      .unwrap()
  );
  let body: Effect<bool, SqlError, HCons<PgPool, HNil>> =
    need::<PgPool>().map(|p: Arc<PgPool>| p.options().get_max_connections() > 0);
  assert!(
    run(body.provide_layer(provide_pg_pool(lazy_pool())))
      .await
      .unwrap()
  );
}

#[tokio::test]
async fn client_layer_fails_its_build_when_server_unreachable() {
  let body: Effect<(), SqlError, PgSqlEnv> = need::<SqlClientService>().map(|_| ());
  let config = PgPoolConfig::from_url("postgres://127.0.0.1:1/postgres").with_max_size(1);
  assert!(
    run(body.provide_layer(PgSqlClient::layer(config)))
      .await
      .is_err()
  );
}

#[tokio::test]
async fn live_postgres_round_trip() {
  let Some(client) = live_client("live_postgres_round_trip").await else {
    return;
  };
  let rows = run(client.query("SELECT $1::int AS n", &[SqlParam::Int(42)]))
    .await
    .expect("query");
  assert_eq!(rows[0].cells.first().map(String::as_str), Some("42"));
  run(client.execute("SELECT 1", &[])).await.expect("execute");
  let tx = run(client.begin()).await.expect("begin");
  run(tx.commit()).await.expect("commit");
}

#[tokio::test]
async fn live_postgres_all_param_types_and_row_decode() {
  let Some(client) = live_client("live_postgres_all_param_types_and_row_decode").await else {
    return;
  };
  let rows = run(client.query(
    "SELECT $1::bool AS b, $2::text AS t, $3::bytea AS bytes, $4::int AS n, NULL::int AS nil, 3.14::float AS f",
    &[
      SqlParam::Bool(true),
      SqlParam::Text("hello".into()),
      SqlParam::Bytes(vec![1, 2, 3]),
      SqlParam::Int(99),
    ],
  ))
  .await
  .expect("typed query");
  assert_eq!(rows[0].cells.get(1).map(String::as_str), Some("hello"));
  assert_eq!(rows[0].cells.get(3).map(String::as_str), Some("99"));
  assert_eq!(rows[0].cells.get(4).map(String::as_str), Some(""));
  assert!(rows[0].cells.get(5).is_some_and(|c| c.starts_with("3.14")));
  let null_rows = run(client.query("SELECT $1::int", &[SqlParam::Null]))
    .await
    .expect("null param query");
  assert_eq!(null_rows.len(), 1);
  assert!(
    run(client.execute("SELECT FROM", &[])).await.is_err(),
    "malformed SQL must fail"
  );
}

#[tokio::test]
async fn live_postgres_double_finish_fails() {
  let Some(client) = live_client("live_postgres_double_finish_fails").await else {
    return;
  };
  let tx = run(client.begin()).await.expect("begin");
  run(tx.commit()).await.expect("first commit");
  assert!(run(tx.commit()).await.is_err());
  assert!(run(tx.rollback()).await.is_err());
  assert!(run(tx.execute("SELECT 1", &[])).await.is_err());
}

#[tokio::test]
async fn live_postgres_transaction_statements_are_isolated_until_commit() {
  let t = "m_sql_tx_isolated";
  let Some(client) = live_client(t).await else {
    return;
  };
  table(&client, t).await;
  let tx = run(client.begin()).await.expect("begin");
  let inserted = run(tx.execute(&format!("INSERT INTO {t} VALUES (1)"), &[]))
    .await
    .expect("insert");
  assert_eq!(inserted, 1);
  let inside = run(tx.query(&format!("SELECT COUNT(*)::int FROM {t}"), &[]))
    .await
    .expect("count in tx");
  assert_eq!(inside[0].cells[0], "1");
  assert_eq!(
    count(&client, t).await,
    "0",
    "uncommitted row visible outside"
  );
  run(tx.commit()).await.expect("commit");
  assert_eq!(count(&client, t).await, "1");
  let tx = run(client.begin()).await.expect("begin 2");
  run(tx.execute(&format!("INSERT INTO {t} VALUES (2)"), &[]))
    .await
    .expect("insert 2");
  run(tx.rollback()).await.expect("rollback");
  assert_eq!(count(&client, t).await, "1");
  drop_table(&client, t).await;
}

#[tokio::test]
async fn live_postgres_with_transaction_commits_and_rolls_back() {
  let t = "m_sql_with_tx";
  let Some(client) = live_client(t).await else {
    return;
  };
  table(&client, t).await;
  let shared: Arc<dyn SqlClient> = Arc::new(client.clone());
  let insert = format!("INSERT INTO {t} VALUES (1)");
  let ins = insert.clone();
  run(with_transaction(Arc::clone(&shared), move |tx| {
    tx.execute(&ins, &[])
  }))
  .await
  .expect("committed");
  assert_eq!(count(&client, t).await, "1");
  let err = run(with_transaction(shared, move |tx| {
    tx.execute(&insert, &[])
      .then(fail::<(), _, _>(SqlError::Unsupported("abort".into())))
  }))
  .await
  .unwrap_err();
  assert_eq!(err, SqlError::Unsupported("abort".into()));
  assert_eq!(
    count(&client, t).await,
    "1",
    "failed transaction was not rolled back"
  );
  drop_table(&client, t).await;
}

fn insert_in_layer(t: &str) -> Effect<u64, SqlError, SqlTransactionEnv> {
  let sql = format!("INSERT INTO {t} VALUES (1)");
  need::<SqlTransactionService>().flat_map(move |tx| tx.execute(&sql, &[]).narrow())
}

#[tokio::test]
async fn live_postgres_scoped_layers_commit_roll_back_and_close_the_pool() {
  let t = "m_sql_layers";
  let Some(client) = live_client(t).await else {
    return;
  };
  let Some(config) = live_config(t) else {
    return;
  };
  table(&client, t).await;

  // Success: committed, and the layer's own pool is closed after the program.
  let pool_seen: Arc<std::sync::Mutex<Option<sqlx::PgPool>>> = Arc::default();
  let seen = Arc::clone(&pool_seen);
  let committed: Effect<u64, SqlError, PgSqlEnv> = need::<PgPool>()
    .flat_map(move |p: Arc<PgPool>| {
      sync(move || {
        *seen.lock().unwrap() = Some(p.0.clone());
      })
    })
    .then(
      insert_in_layer(t)
        .provide_layer(transaction_layer())
        .narrow(),
    );
  let n = run(committed.provide_layer(PgSqlClient::layer(config.clone())))
    .await
    .expect("committed program");
  assert_eq!(n, 1);
  assert_eq!(count(&client, t).await, "1");
  let pool = pool_seen.lock().unwrap().clone().expect("pool seen");
  assert!(
    pool.is_closed(),
    "PgSqlClient::layer did not close its pool"
  );

  // Failure: rolled back.
  let failed: Effect<(), SqlError, PgSqlEnv> = insert_in_layer(t)
    .flat_map(|_| fail(SqlError::Unsupported("abort".into())))
    .provide_layer(transaction_layer())
    .narrow();
  assert!(
    run(failed.provide_layer(PgSqlClient::layer(config.clone())))
      .await
      .is_err()
  );
  assert_eq!(
    count(&client, t).await,
    "1",
    "failed scope was not rolled back"
  );

  // Interrupt: rolled back.
  let inserted = Arc::new(AtomicBool::new(false));
  let flag = Arc::clone(&inserted);
  let hung: Effect<(), SqlError, PgSqlEnv> = insert_in_layer(t)
    .then(sync(move || flag.store(true, Ordering::SeqCst)))
    .then(sleep(Duration::from_secs(3600)))
    .provide_layer(transaction_layer())
    .narrow();
  let rt = TokioRuntime::current().expect("tokio");
  let h = rt.fork(hung.provide_layer(PgSqlClient::layer(config)));
  tokio::time::timeout(Duration::from_secs(10), async {
    while !inserted.load(Ordering::SeqCst) {
      tokio::time::sleep(Duration::from_millis(5)).await;
    }
  })
  .await
  .expect("the insert never ran");
  h.interrupt_now();
  let ex = tokio::time::timeout(Duration::from_secs(10), h.join_future())
    .await
    .expect("the interrupted fiber did not finish");
  assert!(matches!(ex, Exit::Failure(Cause::Interrupt(_))), "{ex:?}");
  assert_eq!(
    count(&client, t).await,
    "1",
    "interrupted scope was not rolled back"
  );

  drop_table(&client, t).await;
}

#[tokio::test]
async fn live_postgres_query_reads_the_client_from_the_layer() {
  let t = "live_postgres_query_reads_the_client_from_the_layer";
  let Some(config) = live_config(t) else {
    return;
  };
  if live_client(t).await.is_none() {
    return;
  }
  let body: Effect<Vec<id_effect_sql::SqlRow>, SqlError, PgSqlEnv> = query("SELECT 7::int", vec![]);
  let rows = run(body.provide_layer(PgSqlClient::layer(config)))
    .await
    .expect("query");
  assert_eq!(rows[0].cells[0], "7");
}
