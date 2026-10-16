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

use id_effect::{Cause, Exit};
use id_effect::{Effect, HCons, HNil, fail, need, sleep, sync};
use id_effect_sql::pg::{
  PgPool, PgPoolConfig, PgSqlClient, PgSqlEnv, pg_pool_from_config, pg_pool_from_config_lazy,
  provide_pg_pool, provide_pg_sql_client,
};
use id_effect_sql::{
  SqlClient, SqlClientService, SqlError, SqlTransactionEnv, SqlTransactionService, SqlType,
  SqlValue, query, transaction_layer, with_transaction,
};
use id_effect_tokio::TokioRuntime;

#[allow(dead_code)]
mod conformance;

use conformance::Backend;

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

async fn count(client: &PgSqlClient, name: &str) -> i32 {
  let rows = run(client.query(&format!("SELECT COUNT(*)::int FROM {name}"), &[]))
    .await
    .expect("count");
  rows[0].get::<i32>(0usize).expect("count is an int4")
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
  let rows = run(client.query("SELECT $1::int AS n", &[SqlValue::I32(42)]))
    .await
    .expect("query");
  assert_eq!(rows[0].get::<i32>("n"), Ok(42));
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
    "SELECT $1::bool AS b, $2::text AS t, $3::bytea AS bytes, $4::int AS n, NULL::int AS nil, 2.5::float AS f",
    &[
      SqlValue::Bool(true),
      SqlValue::Text("hello".into()),
      SqlValue::Bytes(vec![1, 2, 3]),
      SqlValue::I32(99),
    ],
  ))
  .await
  .expect("typed query");
  let row = &rows[0];
  assert_eq!(row.get::<bool>("b"), Ok(true));
  assert_eq!(row.get::<String>("t"), Ok("hello".into()));
  assert_eq!(row.get::<Vec<u8>>("bytes"), Ok(vec![1, 2, 3]));
  assert_eq!(row.get::<i32>("n"), Ok(99));
  assert_eq!(row.get::<Option<i32>>("nil"), Ok(None));
  assert_eq!(row.get::<f64>("f"), Ok(2.5));
  let types: Vec<(SqlType, &str)> = row
    .columns()
    .iter()
    .map(|c| (c.sql_type(), c.db_type()))
    .collect();
  assert_eq!(
    types,
    [
      (SqlType::Bool, "BOOL"),
      (SqlType::Text, "TEXT"),
      (SqlType::Bytes, "BYTEA"),
      (SqlType::I32, "INT4"),
      (SqlType::I32, "INT4"),
      (SqlType::F64, "FLOAT8"),
    ]
  );
  let null_rows = run(client.query("SELECT $1::int", &[SqlValue::Null]))
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
  assert_eq!(inside[0].get::<i32>(0usize), Ok(1));
  assert_eq!(
    count(&client, t).await,
    0,
    "uncommitted row visible outside"
  );
  run(tx.commit()).await.expect("commit");
  assert_eq!(count(&client, t).await, 1);
  let tx = run(client.begin()).await.expect("begin 2");
  run(tx.execute(&format!("INSERT INTO {t} VALUES (2)"), &[]))
    .await
    .expect("insert 2");
  run(tx.rollback()).await.expect("rollback");
  assert_eq!(count(&client, t).await, 1);
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
  assert_eq!(count(&client, t).await, 1);
  let err = run(with_transaction(shared, move |tx| {
    tx.execute(&insert, &[])
      .then(fail::<(), _, _>(SqlError::Unsupported("abort".into())))
  }))
  .await
  .unwrap_err();
  assert_eq!(err, SqlError::Unsupported("abort".into()));
  assert_eq!(
    count(&client, t).await,
    1,
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
  assert_eq!(count(&client, t).await, 1);
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
    1,
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
    1,
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
  assert_eq!(rows[0].get::<i32>(0usize), Ok(7));
}

// ---- typed values (E3.1a): the conformance bodies on PostgreSQL --------------------------------

struct Pg(PgSqlClient);

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

async fn live_pg(test: &str) -> Option<Pg> {
  live_client(test).await.map(Pg)
}

#[tokio::test]
async fn typed_binds_round_trip_uuid_timestamp_decimal_json() {
  let t = "m_sql_typed_binds";
  let Some(pg) = live_pg(t).await else {
    return;
  };
  conformance::typed_binds_round_trip_uuid_timestamp_decimal_json(&pg, t).await;
}

#[tokio::test]
async fn row_get_by_name_and_index() {
  let Some(pg) = live_pg("row_get_by_name_and_index").await else {
    return;
  };
  conformance::row_get_by_name_and_index(&pg).await;
}

#[tokio::test]
async fn deprecated_sqlparam_still_binds() {
  let t = "m_sql_deprecated_param";
  let Some(pg) = live_pg(t).await else {
    return;
  };
  conformance::deprecated_sqlparam_still_binds(&pg, t).await;
}

#[tokio::test]
async fn untyped_null_binds_into_int_text_uuid_columns() {
  let t = "m_sql_untyped_null";
  let Some(pg) = live_pg(t).await else {
    return;
  };
  conformance::untyped_null_binds_into_int_text_uuid_columns(&pg, t).await;
}

#[tokio::test]
async fn unsupported_column_type_is_a_clear_error() {
  let Some(pg) = live_pg("unsupported_column_type_is_a_clear_error").await else {
    return;
  };
  let err = run(pg.0.query("SELECT '127.0.0.1'::inet AS addr", &[]))
    .await
    .unwrap_err();
  let SqlError::Unsupported(message) = err else {
    panic!("expected Unsupported, got {err:?}");
  };
  assert!(message.contains("`addr`"), "{message}");
  assert!(message.contains("INET"), "{message}");
  assert!(message.contains("addr::text"), "{message}");
  // The suggested cast works.
  let rows = run(pg.0.query("SELECT '127.0.0.1'::inet::text AS addr", &[]))
    .await
    .expect("cast");
  assert_eq!(rows[0].get::<String>("addr"), Ok("127.0.0.1/32".into()));
}

#[tokio::test]
async fn enums_oids_and_out_of_range_values_decode_or_fail_by_column() {
  let t = "enums_oids_and_out_of_range_values_decode_or_fail_by_column";
  let Some(pg) = live_pg(t).await else {
    return;
  };
  let c = &pg.0;
  run(c.execute("DROP TYPE IF EXISTS m_sql_mood", &[]))
    .await
    .expect("drop type");
  run(c.execute("CREATE TYPE m_sql_mood AS ENUM ('happy', 'sad')", &[]))
    .await
    .expect("create type");
  let rows = run(c.query(
    "SELECT 'happy'::m_sql_mood AS mood, 'pg_class'::regclass::oid AS o, NULL::m_sql_mood AS none",
    &[],
  ))
  .await
  .expect("enum and oid");
  assert_eq!(rows[0].get::<String>("mood"), Ok("happy".into()));
  assert_eq!(rows[0].columns()[0].db_type(), "m_sql_mood");
  assert!(rows[0].get::<i64>("o").is_ok_and(|o| o > 0));
  assert_eq!(rows[0].get::<Option<String>>("none"), Ok(None));
  run(c.execute("DROP TYPE m_sql_mood", &[]))
    .await
    .expect("drop type");
  for (sql, column) in [
    ("SELECT 'infinity'::timestamptz AS at", "at"),
    ("SELECT '-infinity'::timestamp AS local", "local"),
    ("SELECT 'infinity'::date AS day", "day"),
    ("SELECT '24:00:00'::time AS tod", "tod"),
    ("SELECT 'NaN'::numeric AS amount", "amount"),
  ] {
    let err = run(c.query(sql, &[])).await.unwrap_err();
    assert!(
      matches!(&err, SqlError::ValueOutOfRange { column: c, .. } if c == column),
      "{sql}: {err:?}"
    );
  }
}

#[cfg(feature = "proptest")]
#[test]
fn prop_values_round_trip_through_insert_select() {
  use id_effect_sql::testing::arb_sql_value;
  use proptest::test_runner::{Config, TestCaseError, TestRunner};

  let t = "m_sql_prop_values";
  let rt = tokio::runtime::Runtime::new().expect("tokio");
  let Some(pg) = rt.block_on(live_pg(t)) else {
    return;
  };
  rt.block_on(conformance::create_value_table(&pg, t));
  let mut runner = TestRunner::new(Config {
    cases: 64,
    failure_persistence: None,
    ..Config::default()
  });
  let outcome = runner.run(&arb_sql_value(), |value| {
    rt.block_on(conformance::value_round_trips(&pg, t, value))
      .map_err(TestCaseError::fail)
  });
  rt.block_on(conformance::drop_table(&pg, t));
  outcome.expect("every generated value round-trips through PostgreSQL");
}
