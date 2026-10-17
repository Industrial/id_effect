//! Tests for the `sqlite` feature beyond the shared conformance suite (`tests/conformance`):
//! database files and in-memory databases, the scoped layer, how transaction options map onto
//! SQLite, busy databases, value decoding by storage class, stream release, and read-replica
//! routing over real databases. No server is needed.
//!
//! Effects run on `id_effect_tokio::TokioRuntime` (sqlx needs tokio).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use id_effect::{Cause, Effect, Exit, Never, Schedule, need, sleep, succeed};
use id_effect_sql::jiff::civil::date;
use id_effect_sql::sqlite::{
  SqlitePoolConfig, SqliteSqlClient, sqlite_pool_from_config, sqlite_pool_from_config_lazy,
};
use id_effect_sql::{
  Isolation, RoutingSqlClient, SqlClient, SqlClientEnv, SqlClientService, SqlError, SqlRow,
  SqlType, SqlValue, TransactionOptions, is_serialization_failure, params,
  retry_serialization_failures, sql_client_layer, with_transaction, with_transaction_options,
};
use id_effect_tokio::TokioRuntime;

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

async fn memory_db() -> SqliteSqlClient {
  let pool = sqlite_pool_from_config(SqlitePoolConfig::memory())
    .await
    .expect("in-memory pool");
  SqliteSqlClient::new(pool)
}

/// A database file path under the temp dir, deleted (with its WAL files) on drop.
struct TempDb(PathBuf);

impl TempDb {
  fn new(name: &str) -> Self {
    let path = std::env::temp_dir().join(format!(
      "id_effect_sql_test_{name}_{}.sqlite",
      std::process::id()
    ));
    let db = Self(path);
    db.remove();
    db
  }

  fn path(&self) -> &Path {
    &self.0
  }

  fn remove(&self) {
    for suffix in ["", "-wal", "-shm"] {
      let mut path = self.0.clone().into_os_string();
      path.push(suffix);
      let _ = std::fs::remove_file(path);
    }
  }
}

impl Drop for TempDb {
  fn drop(&mut self) {
    self.remove();
  }
}

async fn file_db(db: &TempDb, busy: Duration) -> SqliteSqlClient {
  let config = SqlitePoolConfig::file(db.path()).with_busy_timeout(busy);
  SqliteSqlClient::new(sqlite_pool_from_config(config).await.expect("file pool"))
}

async fn ok<A: Send + 'static>(eff: Effect<A, SqlError>) -> A {
  run(eff).await.expect("statement")
}

async fn count(client: &dyn SqlClient, table: &str) -> i64 {
  let rows = ok(client.query(&format!("SELECT COUNT(*) AS n FROM {table}"), &[])).await;
  rows[0].get::<i64>("n").expect("count")
}

async fn numbers_table(client: &dyn SqlClient) {
  ok(client.execute("CREATE TABLE numbers (n INTEGER)", &[])).await;
}

#[tokio::test]
async fn database_files_persist_and_in_memory_databases_are_private() {
  let db = TempDb::new("persist");
  let client = file_db(&db, Duration::from_secs(5)).await;
  numbers_table(&client).await;
  ok(client.execute("INSERT INTO numbers VALUES (?1)", &params![7i64])).await;
  let mode = ok(client.query("PRAGMA journal_mode", &[])).await;
  assert_eq!(mode[0].get::<String>(0usize), Ok("wal".into()));
  client.pool().close().await;
  // Reopened by URL, without creating: the row is still there.
  let url = SqlitePoolConfig::file(db.path()).url;
  let reopened = SqliteSqlClient::new(
    sqlite_pool_from_config(SqlitePoolConfig::from_url(url))
      .await
      .expect("reopen"),
  );
  assert_eq!(count(&reopened, "numbers").await, 1);

  let (a, b) = (memory_db().await, memory_db().await);
  numbers_table(&a).await;
  ok(a.execute("INSERT INTO numbers VALUES (1)", &[])).await;
  assert_eq!(count(&a, "numbers").await, 1);
  assert!(
    run(b.query("SELECT n FROM numbers", &[])).await.is_err(),
    "each in-memory pool is its own database"
  );
}

#[tokio::test]
async fn unreachable_databases_fail_at_connect_and_build() {
  let url = "sqlite:///nonexistent-id-effect-dir/x.sqlite";
  let err = sqlite_pool_from_config(SqlitePoolConfig::from_url(url))
    .await
    .unwrap_err();
  assert!(matches!(err.0, SqlError::QueryFailed { .. }), "{err:?}");
  let lazy = sqlite_pool_from_config_lazy(SqlitePoolConfig::from_url(url)).expect("lazy pool");
  let client = SqliteSqlClient::new(lazy);
  assert!(run(client.connect()).await.is_err());
  assert!(run(client.query("SELECT 1", &[])).await.is_err());
  assert!(run(client.execute("SELECT 1", &[])).await.is_err());
  assert!(run(client.begin()).await.is_err());
  assert!(sqlite_pool_from_config_lazy(SqlitePoolConfig::from_url("sqlite://x?mode=bad")).is_err());
  let body: Effect<(), SqlError, SqlClientEnv> = need::<SqlClientService>().map(|_| ());
  let built = run(body.provide_layer(SqliteSqlClient::layer(SqlitePoolConfig::from_url(url))));
  assert!(built.await.is_err());
}

#[tokio::test]
async fn the_layer_provides_a_client_and_closes_its_database() {
  let body: Effect<Vec<SqlRow>, SqlError, SqlClientEnv> =
    need::<SqlClientService>().flat_map(|db| {
      let read = Arc::clone(&db.0);
      db.execute("CREATE TABLE t (n INTEGER)", &[])
        .then(db.execute("INSERT INTO t VALUES (?1)", &params![5i64]))
        .then(read.query("SELECT n FROM t", &[]))
        .narrow()
    });
  let rows = run(body.provide_layer(SqliteSqlClient::layer(SqlitePoolConfig::memory())))
    .await
    .expect("program");
  assert_eq!(rows[0].get::<i64>("n"), Ok(5));
  // The generic scoped layer closes the pool through `SqlClient::close`.
  let client = memory_db().await;
  let pool = client.pool().clone();
  let body: Effect<(), SqlError, SqlClientEnv> = need::<SqlClientService>().map(|_| ());
  run(body.provide_layer(sql_client_layer(client.clone())))
    .await
    .expect("program");
  assert!(pool.is_closed());
  for failed in [
    run(client.query("SELECT 1", &[]).map(|_| ())).await,
    run(client.execute("SELECT 1", &[]).map(|_| ())).await,
    run(client.begin().map(|_| ())).await,
    run(
      client
        .query_stream("SELECT 1", &[])
        .run_collect()
        .map(|_| ()),
    )
    .await,
  ] {
    assert_eq!(failed, Err(SqlError::NotConnected));
  }
}

#[tokio::test]
async fn wal_readers_do_not_wait_for_an_open_writer() {
  let db = TempDb::new("wal_readers");
  let client = file_db(&db, Duration::from_secs(5)).await;
  numbers_table(&client).await;
  let tx = ok(client.begin()).await;
  ok(tx.execute("INSERT INTO numbers VALUES (1)", &[])).await;
  assert_eq!(
    count(&client, "numbers").await,
    0,
    "uncommitted row visible"
  );
  ok(tx.commit()).await;
  assert_eq!(count(&client, "numbers").await, 1);
}

#[tokio::test]
async fn serializable_takes_the_write_lock_and_busy_is_a_serialization_failure() {
  let db = TempDb::new("busy");
  let client = file_db(&db, Duration::from_millis(50)).await;
  numbers_table(&client).await;
  // `BEGIN IMMEDIATE` holds the write lock from the start ...
  let holder = ok(client.begin_with(Isolation::Serializable.into())).await;
  let err = run(client.begin_with(Isolation::Serializable.into()))
    .await
    .err()
    .expect("second writer must wait, then fail");
  assert!(is_serialization_failure(&err), "{err:?}");
  let err = run(client.execute("INSERT INTO numbers VALUES (1)", &[]))
    .await
    .unwrap_err();
  assert!(is_serialization_failure(&err), "{err:?}");
  // ... while deferred transactions take no lock until they write.
  let deferred = ok(client.begin_with(Isolation::ReadCommitted.into())).await;
  assert_eq!(ok(deferred.query("SELECT 1 AS n", &[])).await.len(), 1);
  ok(deferred.rollback()).await;
  ok(holder.rollback()).await;

  // A busy transaction is retried as a serialization failure: the first attempt meets the
  // holder's lock, the holder lets go, the second attempt commits.
  let holder = ok(client.begin_with(Isolation::Serializable.into())).await;
  let attempts = Arc::new(AtomicUsize::new(0));
  let (shared, tries): (Arc<dyn SqlClient>, _) = (Arc::new(client.clone()), Arc::clone(&attempts));
  let post = retry_serialization_failures(Schedule::recurs(3), move || {
    let release = if tries.fetch_add(1, Ordering::SeqCst) == 1 {
      holder.rollback()
    } else {
      succeed(())
    };
    let insert = with_transaction_options(Arc::clone(&shared), Isolation::Serializable, |tx| {
      tx.execute("INSERT INTO numbers VALUES (2)", &[])
    });
    release.then(insert)
  });
  assert_eq!(run(post).await, Ok(1));
  assert_eq!(attempts.load(Ordering::SeqCst), 2);
  assert_eq!(count(&client, "numbers").await, 1);
}

#[tokio::test]
async fn read_only_is_unsupported_and_deferrable_changes_nothing() {
  let client = memory_db().await;
  let err = run(client.begin_with(TransactionOptions::new().with_read_only()))
    .await
    .err()
    .expect("read only");
  assert!(
    matches!(&err, SqlError::Unsupported(m) if m.contains("mode=ro")),
    "{err:?}"
  );
  let tx = ok(client.begin_with(TransactionOptions::new().with_deferrable())).await;
  ok(tx.commit()).await;
  // A finished handle refuses savepoints too.
  assert!(matches!(
    run(tx.create_savepoint("late")).await,
    Err(SqlError::TransactionFailed(_))
  ));
}

#[tokio::test]
async fn values_decode_by_storage_class_and_declared_type() {
  let client = memory_db().await;
  let rows = ok(client.query(
    "SELECT 1 AS i, 1.5 AS r, 'x' AS t, x'00ff' AS b, NULL AS z",
    &[],
  ))
  .await;
  assert_eq!(
    rows[0].values(),
    [
      SqlValue::I64(1),
      SqlValue::F64(1.5),
      SqlValue::Text("x".into()),
      SqlValue::Bytes(vec![0, 255]),
      SqlValue::Null
    ]
  );
  let columns: Vec<(SqlType, &str)> = rows[0]
    .columns()
    .iter()
    .map(|c| (c.sql_type(), c.db_type()))
    .collect();
  assert_eq!(
    columns,
    [
      (SqlType::I64, "INTEGER"),
      (SqlType::F64, "REAL"),
      (SqlType::Text, "TEXT"),
      (SqlType::Bytes, "BLOB"),
      (SqlType::Null, "NULL")
    ]
  );

  ok(client.execute(
    "CREATE TABLE v (ok BOOLEAN, d DATE, tm TIME, at DATETIME, n INTEGER, s TEXT, \
     made DATETIME DEFAULT CURRENT_TIMESTAMP)",
    &[],
  ))
  .await;
  // Text is never parsed into numbers or booleans; integers in a TEXT column become text.
  ok(client.execute(
    "INSERT INTO v (ok, d, tm, at, n, s) VALUES ('true', NULL, '12:30:00', '2024-01-01 10:00:00', 'abc', 5)",
    &[],
  ))
  .await;
  ok(client.execute(
    "INSERT INTO v (ok, at, n) VALUES (1, '2024-01-01T10:00:00+02:00', 7)",
    &[],
  ))
  .await;
  let rows = ok(client.query("SELECT ok, d, tm, at, n, s, made FROM v", &[])).await;
  assert_eq!(rows[0].get::<String>("ok"), Ok("true".into()));
  assert!(matches!(
    rows[0].get::<bool>("ok"),
    Err(SqlError::TypeMismatch {
      found: SqlType::Text,
      ..
    })
  ));
  assert_eq!(rows[1].get::<bool>("ok"), Ok(true));
  assert_eq!(rows[0].value("d"), Ok(&SqlValue::Null));
  assert_eq!(
    rows[0].columns()[1].sql_type(),
    SqlType::Date,
    "all NULL: declared"
  );
  assert_eq!(
    rows[0]
      .get::<id_effect_sql::jiff::civil::Time>("tm")
      .map(|t| t.minute()),
    Ok(30)
  );
  assert_eq!(
    rows[0].value("at"),
    Ok(&SqlValue::DateTime(date(2024, 1, 1).at(10, 0, 0, 0)))
  );
  assert_eq!(
    rows[1].value("at"),
    Ok(&SqlValue::Timestamp(
      "2024-01-01T08:00:00Z".parse().expect("timestamp")
    ))
  );
  assert_eq!(rows[0].get::<String>("n"), Ok("abc".into()));
  assert_eq!(rows[1].get::<i64>("n"), Ok(7));
  assert_eq!(
    rows[0].columns()[4].sql_type(),
    SqlType::Text,
    "first non-NULL"
  );
  assert_eq!(rows[0].get::<String>("s"), Ok("5".into()));
  assert!(matches!(rows[0].value("made"), Ok(SqlValue::DateTime(_))));

  // Values a declared type cannot hold fail naming the column.
  ok(client.execute("UPDATE v SET ok = 2 WHERE n = 7", &[])).await;
  let err = run(client.query("SELECT ok FROM v WHERE n = 7", &[]))
    .await
    .unwrap_err();
  assert!(
    matches!(&err, SqlError::ValueOutOfRange { column, message } if column == "ok" && message.contains('2')),
    "{err:?}"
  );
  ok(client.execute("UPDATE v SET d = 'someday' WHERE n = 7", &[])).await;
  let err = run(client.query("SELECT d FROM v WHERE n = 7", &[]))
    .await
    .unwrap_err();
  assert!(
    matches!(&err, SqlError::Decode(m) if m.contains("column `d`")),
    "{err:?}"
  );
  // So do binds beyond the text form's years 0000-9999.
  for value in [
    SqlValue::Date(date(-1, 1, 1)),
    SqlValue::DateTime(date(-1, 1, 1).at(0, 0, 0, 0)),
    SqlValue::Timestamp(id_effect_sql::jiff::Timestamp::MIN),
  ] {
    let err = run(client.query("SELECT ?1 AS x", std::slice::from_ref(&value)))
      .await
      .unwrap_err();
    assert!(
      matches!(&err, SqlError::ValueOutOfRange { column, .. } if column == "$1"),
      "{value:?}: {err:?}"
    );
  }
}

#[tokio::test]
async fn interrupted_and_dropped_streams_release_their_connection() {
  let pool = sqlite_pool_from_config(SqlitePoolConfig::memory().with_max_size(1))
    .await
    .expect("pool");
  let client = SqliteSqlClient::new(pool);
  let sql = "WITH RECURSIVE s(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM s WHERE n < 10000000) \
             SELECT n FROM s";
  let seen = Arc::new(AtomicUsize::new(0));
  let rows_seen = Arc::clone(&seen);
  // The consumer takes one row and stalls: backpressure keeps the statement open.
  let consumer = client.query_stream(sql, &[]).run_for_each(move |_| {
    rows_seen.fetch_add(1, Ordering::SeqCst);
    sleep(Duration::from_secs(3600))
  });
  let rt = TokioRuntime::current().expect("tokio");
  let h = rt.fork(consumer);
  tokio::time::timeout(Duration::from_secs(10), async {
    while seen.load(Ordering::SeqCst) == 0 {
      tokio::time::sleep(Duration::from_millis(5)).await;
    }
  })
  .await
  .expect("no row arrived");
  h.interrupt_now();
  let ex = tokio::time::timeout(Duration::from_secs(10), h.join_future())
    .await
    .expect("the interrupted consumer did not finish");
  assert!(matches!(ex, Exit::Failure(Cause::Interrupt(_))), "{ex:?}");
  assert_eq!(seen.load(Ordering::SeqCst), 1);
  // The only connection is back in the pool.
  let rows = tokio::time::timeout(Duration::from_secs(10), run(client.query("SELECT 1", &[])))
    .await
    .expect("the stream's connection was not released")
    .expect("query");
  assert_eq!(rows.len(), 1);
  let (_, rest) = ok(client.query_stream(sql, &[]).next_chunk())
    .await
    .expect("rows");
  drop(rest);
  let rows = tokio::time::timeout(Duration::from_secs(10), run(client.query("SELECT 2", &[])))
    .await
    .expect("the dropped stream's connection was not released")
    .expect("query");
  assert_eq!(rows.len(), 1);
}

#[tokio::test]
async fn stream_failures_end_the_stream() {
  let client = memory_db().await;
  ok(client.execute("CREATE TABLE flags (ok BOOLEAN)", &[])).await;
  ok(client.execute("INSERT INTO flags VALUES (1), (3)", &[])).await;
  let err = run(
    client
      .query_stream("SELECT ok FROM flags", &[])
      .run_collect(),
  )
  .await
  .unwrap_err();
  assert!(matches!(err, SqlError::ValueOutOfRange { .. }), "{err:?}");
  let err = run(
    client
      .query_stream("SELECT ?1 AS d", &[SqlValue::Date(date(-1, 1, 1))])
      .run_collect(),
  )
  .await
  .unwrap_err();
  assert!(matches!(err, SqlError::ValueOutOfRange { .. }), "{err:?}");
  // A runtime error after some rows: the rows come first, then the failure.
  let err = run(
    client
      .query_stream(
        "SELECT CASE WHEN ok = 3 THEN json('not json') ELSE ok END AS v FROM flags",
        &[],
      )
      .run_collect(),
  )
  .await
  .unwrap_err();
  assert!(matches!(err, SqlError::QueryFailed { .. }), "{err:?}");
}

#[tokio::test]
async fn query_stream_without_a_tokio_runtime_is_unsupported() {
  // The pool lives (and is dropped) on the runtime; the stream is pulled on a plain thread.
  let client = memory_db().await;
  let pool = client.pool().clone();
  let exit = std::thread::spawn(move || {
    client
      .query_stream("SELECT 1", &[])
      .run_collect()
      .block_on()
  })
  .join()
  .expect("thread");
  drop(pool);
  assert!(
    matches!(&exit, Exit::Failure(Cause::Fail(SqlError::Unsupported(m))) if m.contains("tokio")),
    "{exit:?}"
  );
}

#[tokio::test]
async fn reads_go_to_replica_writes_to_primary() {
  let (primary, replica) = (memory_db().await, memory_db().await);
  for db in [&primary, &replica] {
    numbers_table(db).await;
  }
  ok(replica.execute("INSERT INTO numbers VALUES (100), (200)", &[])).await;
  let router = RoutingSqlClient::new(Arc::new(primary.clone()), vec![Arc::new(replica.clone())]);
  ok(router.connect()).await;
  assert_eq!(
    ok(router.execute("INSERT INTO numbers VALUES (?1)", &params![1i64])).await,
    1
  );
  assert_eq!(
    count(&primary, "numbers").await,
    1,
    "the write is the primary's"
  );
  assert_eq!(count(&replica, "numbers").await, 2);
  // Reads (and streams) see the replica.
  assert_eq!(count(&router, "numbers").await, 2);
  let streamed = ok(
    router
      .query_stream("SELECT n FROM numbers", &[])
      .run_collect(),
  )
  .await;
  assert_eq!(streamed.len(), 2);
  // A transaction runs on the primary, reads included.
  let shared: Arc<dyn SqlClient> = Arc::new(router.clone());
  let inside = ok(with_transaction(shared, |tx| {
    tx.execute("INSERT INTO numbers VALUES (2)", &[])
      .then(tx.query("SELECT COUNT(*) AS n FROM numbers", &[]))
  }))
  .await;
  assert_eq!(inside[0].get::<i64>("n"), Ok(2));
  assert_eq!(count(&**router.primary(), "numbers").await, 2);
  ok(router.close().map_err(|n: Never| match n {})).await;
  assert!(primary.pool().is_closed() && replica.pool().is_closed());
}
