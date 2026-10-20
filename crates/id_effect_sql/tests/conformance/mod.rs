//! Driver conformance suite: the same behaviour checked on every backend, written against
//! `Arc<dyn SqlClient>` and a [`Backend`] that knows its DDL, placeholders and how its columns
//! read back. [`conformance_suite!`] turns the bodies into one test each for a backend; the
//! `conformance` test target (`main.rs`) instantiates it per backend feature (`pg.rs`,
//! `sqlite.rs`, `mysql.rs`). A new driver adds a `Backend` impl, a `mod` line in `main.rs` and
//! one `conformance_suite!` call. `tests/pg.rs` also calls a few bodies directly.
//!
//! Each body takes a table name owned by the calling test (tests run concurrently).

use std::sync::Arc;

use id_effect::{Cause, Effect, Exit, fail, succeed};
use id_effect_sql::jiff::civil::{Date, DateTime, Time, date, time};
use id_effect_sql::rust_decimal::Decimal;
use id_effect_sql::serde_json::{self, json};
use id_effect_sql::uuid::Uuid;
use id_effect_sql::{
  Isolation, Json, SqlClient, SqlError, SqlRow, SqlTransaction, SqlType, SqlValue, params,
  savepoint, with_transaction, with_transaction_options,
};
use id_effect_tokio::TokioRuntime;

/// One database under test.
pub trait Backend {
  /// A connected client.
  fn client(&self) -> Arc<dyn SqlClient>;
  /// The column type a value of `t` is stored in.
  fn column_ddl(&self, t: SqlType) -> &'static str;
  /// The `n`-th (1-based) parameter placeholder.
  fn placeholder(&self, n: usize) -> String;
  /// The type a column of type `t` reads back as (SQLite widens integers to `I64`, …).
  fn stored_type(&self, t: SqlType) -> SqlType {
    t
  }
  /// `value` as it reads back from the column of its type ([`Self::stored_type`]).
  fn stored_value(&self, value: SqlValue) -> SqlValue {
    value
  }
  /// The most connections the client's pool opens (the stream bodies check none leaks).
  fn max_connections(&self) -> usize {
    4
  }
  /// The type `CAST(1 AS …)` names for a 64-bit integer (MySQL has no `CAST(… AS BIGINT)`).
  fn bigint_cast(&self) -> &'static str {
    "BIGINT"
  }
  /// Whether the column of `value`'s type holds `value` ([`value_round_trips`] skips the rest:
  /// a MySQL `TIMESTAMP` holds years 1970-2038 only).
  fn stores(&self, _value: &SqlValue) -> bool {
    true
  }
}

pub async fn run<A: Send + 'static>(eff: Effect<A, SqlError>) -> Result<A, SqlError> {
  match TokioRuntime::current().expect("tokio").run(eff).await {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
  }
}

pub async fn execute(b: &impl Backend, sql: &str, params: &[SqlValue]) -> u64 {
  run(b.client().execute(sql, params))
    .await
    .unwrap_or_else(|e| panic!("{sql}: {e}"))
}

pub async fn query(b: &impl Backend, sql: &str, params: &[SqlValue]) -> Vec<SqlRow> {
  run(b.client().query(sql, params))
    .await
    .unwrap_or_else(|e| panic!("{sql}: {e}"))
}

/// A fresh `table` with the given columns.
pub async fn create(b: &impl Backend, table: &str, columns: &[(&str, SqlType)]) {
  let defs: Vec<String> = columns
    .iter()
    .map(|(name, t)| format!("{name} {}", b.column_ddl(*t)))
    .collect();
  execute(b, &format!("DROP TABLE IF EXISTS {table}"), &[]).await;
  execute(
    b,
    &format!("CREATE TABLE {table} ({})", defs.join(", ")),
    &[],
  )
  .await;
}

pub async fn drop_table(b: &impl Backend, table: &str) {
  execute(b, &format!("DROP TABLE IF EXISTS {table}"), &[]).await;
}

/// `INSERT INTO table (c1, …) VALUES (p1, …)`.
fn insert_sql(b: &impl Backend, table: &str, columns: &[&str]) -> String {
  let placeholders: Vec<String> = (1..=columns.len()).map(|n| b.placeholder(n)).collect();
  format!(
    "INSERT INTO {table} ({}) VALUES ({})",
    columns.join(", "),
    placeholders.join(", ")
  )
}

#[derive(serde::Deserialize, Debug, PartialEq)]
struct Doc {
  tags: Vec<String>,
  n: i32,
}

pub async fn typed_binds_round_trip_uuid_timestamp_decimal_json(b: &impl Backend, table: &str) {
  let columns = [
    ("id", SqlType::Uuid),
    ("at", SqlType::Timestamp),
    ("local", SqlType::DateTime),
    ("day", SqlType::Date),
    ("tod", SqlType::Time),
    ("amount", SqlType::Decimal),
    ("doc", SqlType::Json),
    ("big", SqlType::I64),
    ("small", SqlType::I16),
    ("mid", SqlType::I32),
    ("ratio", SqlType::F64),
    ("half", SqlType::F32),
    ("ok", SqlType::Bool),
    ("name", SqlType::Text),
    ("raw", SqlType::Bytes),
  ];
  create(b, table, &columns).await;
  let id = Uuid::from_u128(0x0123_4567_89ab_cdef_0123_4567_89ab_cdef);
  let at: id_effect_sql::jiff::Timestamp = "2024-02-29T12:34:56.123456Z".parse().unwrap();
  let local: DateTime = date(1999, 12, 31).at(23, 59, 59, 999_999_000);
  let day: Date = date(1, 1, 1);
  let tod: Time = time(0, 0, 0, 1_000);
  let amount = Decimal::new(-123_456_789_012_345, 6);
  let doc = json!({"tags": ["a", "b"], "n": 7});
  let names: Vec<&str> = columns.iter().map(|(n, _)| *n).collect();
  let inserted = execute(
    b,
    &insert_sql(b, table, &names),
    &params![
      id,
      at,
      local,
      day,
      tod,
      amount,
      doc,
      i64::MIN,
      i16::MAX,
      -7i32,
      0.1f64,
      1.5f32,
      true,
      "ünïcode",
      vec![0u8, 255]
    ],
  )
  .await;
  assert_eq!(inserted, 1);
  let rows = query(
    b,
    &format!(
      "SELECT {} FROM {table} WHERE id = {}",
      names.join(", "),
      b.placeholder(1)
    ),
    &params![id],
  )
  .await;
  let row = &rows[0];
  assert_eq!(row.get::<Uuid>("id"), Ok(id));
  assert_eq!(row.get::<id_effect_sql::jiff::Timestamp>("at"), Ok(at));
  assert_eq!(row.get::<DateTime>("local"), Ok(local));
  assert_eq!(row.get::<Date>("day"), Ok(day));
  assert_eq!(row.get::<Time>("tod"), Ok(tod));
  assert_eq!(row.get::<Decimal>("amount"), Ok(amount));
  assert_eq!(row.get::<serde_json::Value>("doc"), Ok(doc));
  assert_eq!(
    row.get::<Json<Doc>>("doc"),
    Ok(Json(Doc {
      tags: vec!["a".into(), "b".into()],
      n: 7
    }))
  );
  assert_eq!(row.get::<i64>("big"), Ok(i64::MIN));
  assert_eq!(row.get::<i16>("small"), Ok(i16::MAX));
  assert_eq!(row.get::<i64>("mid"), Ok(-7), "integers widen");
  assert_eq!(row.get::<f64>("ratio"), Ok(0.1));
  assert_eq!(row.get::<f32>("half"), Ok(1.5));
  assert_eq!(row.get::<bool>("ok"), Ok(true));
  assert_eq!(row.get::<String>("name"), Ok("ünïcode".into()));
  assert_eq!(row.get::<Vec<u8>>("raw"), Ok(vec![0, 255]));
  let types: Vec<SqlType> = row.columns().iter().map(|c| c.sql_type()).collect();
  let declared: Vec<SqlType> = columns.iter().map(|(_, t)| b.stored_type(*t)).collect();
  assert_eq!(types, declared);
  drop_table(b, table).await;
}

pub async fn row_get_by_name_and_index(b: &impl Backend) {
  let sql = format!(
    "SELECT CAST(1 AS {}) AS a, 'x' AS b, NULL AS c",
    b.bigint_cast()
  );
  let rows = query(b, &sql, &[]).await;
  let row = &rows[0];
  assert_eq!(row.len(), 3);
  assert_eq!(row.get::<i64>("a"), row.get::<i64>(0usize));
  assert_eq!(row.get::<String>("b"), Ok("x".into()));
  assert_eq!(row.get::<String>(1usize), Ok("x".into()));
  assert_eq!(row.get::<Option<String>>("c"), Ok(None));
  assert_eq!(row.value("c"), Ok(&SqlValue::Null));
  assert_eq!(
    row.get::<i64>("missing"),
    Err(SqlError::ColumnNotFound {
      column: "missing".into()
    })
  );
  assert_eq!(
    row.get::<i64>(3usize),
    Err(SqlError::ColumnNotFound {
      column: "#3".into()
    })
  );
  assert!(matches!(
    row.get::<i64>("b"),
    Err(SqlError::TypeMismatch { column, expected: SqlType::I64, found: SqlType::Text }) if column == "b"
  ));
  assert_eq!(
    row.get::<bool>("c"),
    Err(SqlError::UnexpectedNull { column: "c".into() })
  );
  assert!(!row.columns()[0].db_type().is_empty());
}

pub async fn untyped_null_binds_into_int_text_uuid_columns(b: &impl Backend, table: &str) {
  let columns = [
    ("n", SqlType::I32),
    ("s", SqlType::Text),
    ("u", SqlType::Uuid),
    ("at", SqlType::Timestamp),
    ("doc", SqlType::Json),
  ];
  create(b, table, &columns).await;
  let names: Vec<&str> = columns.iter().map(|(n, _)| *n).collect();
  execute(
    b,
    &insert_sql(b, table, &names),
    &params![
      None::<i32>,
      None::<String>,
      None::<Uuid>,
      SqlValue::Null,
      None::<serde_json::Value>
    ],
  )
  .await;
  // NULL also binds where the type comes from a comparison.
  let rows = query(
    b,
    &format!(
      "SELECT n, s, u, at, doc FROM {table} WHERE u IS NULL OR u = {}",
      b.placeholder(1)
    ),
    &[SqlValue::Null],
  )
  .await;
  assert_eq!(rows.len(), 1);
  assert!(rows[0].values().iter().all(SqlValue::is_null));
  assert_eq!(rows[0].get::<Option<i32>>("n"), Ok(None));
  assert_eq!(rows[0].get::<Option<Uuid>>("u"), Ok(None));
  let id = Uuid::from_u128(9);
  let updated = execute(
    b,
    &format!(
      "UPDATE {table} SET u = {}, n = {}",
      b.placeholder(1),
      b.placeholder(2)
    ),
    &params![Some(id), None::<i64>],
  )
  .await;
  assert_eq!(updated, 1);
  let rows = query(b, &format!("SELECT u, n FROM {table}"), &[]).await;
  assert_eq!(rows[0].get::<Option<Uuid>>("u"), Ok(Some(id)));
  assert_eq!(rows[0].get::<Option<i32>>("n"), Ok(None));
  drop_table(b, table).await;
}

/// The column of `table` (see [`create_value_table`]) that holds values of type `t`.
fn value_column(t: SqlType) -> String {
  format!("c_{t:?}").to_lowercase()
}

/// Every portable type that has a column.
pub const VALUE_TYPES: [SqlType; 15] = [
  SqlType::Bool,
  SqlType::I16,
  SqlType::I32,
  SqlType::I64,
  SqlType::F32,
  SqlType::F64,
  SqlType::Text,
  SqlType::Bytes,
  SqlType::Uuid,
  SqlType::Timestamp,
  SqlType::DateTime,
  SqlType::Date,
  SqlType::Time,
  SqlType::Decimal,
  SqlType::Json,
];

/// A table with one column per [`VALUE_TYPES`] entry.
pub async fn create_value_table(b: &impl Backend, table: &str) {
  let names: Vec<String> = VALUE_TYPES.iter().map(|t| value_column(*t)).collect();
  let columns: Vec<(&str, SqlType)> = names.iter().map(String::as_str).zip(VALUE_TYPES).collect();
  create(b, table, &columns).await;
}

/// Inserts `value` into the column of its type (a NULL into the text column), reads it back,
/// and compares with [`Backend::stored_value`].
pub async fn value_round_trips(
  b: &impl Backend,
  table: &str,
  value: SqlValue,
) -> Result<(), String> {
  if !b.stores(&value) {
    return Ok(());
  }
  let t = match value.sql_type() {
    SqlType::Null => SqlType::Text,
    t => t,
  };
  let column = value_column(t);
  execute(b, &format!("DELETE FROM {table}"), &[]).await;
  execute(
    b,
    &insert_sql(b, table, &[column.as_str()]),
    std::slice::from_ref(&value),
  )
  .await;
  let rows = query(b, &format!("SELECT {column} FROM {table}"), &[]).await;
  let back = rows[0].value(0usize).map_err(|e| e.to_string())?;
  if *back == b.stored_value(value.clone()) {
    Ok(())
  } else {
    Err(format!("{value:?} came back as {back:?}"))
  }
}

// ---- statements, transactions, savepoints, isolation, streaming (E3.1b) -----------------------

/// The `n` column of every row.
fn ns(rows: &[SqlRow]) -> Vec<i64> {
  rows
    .iter()
    .map(|r| r.get::<i64>("n").expect("n is an integer"))
    .collect()
}

pub async fn count(b: &impl Backend, table: &str) -> i64 {
  let rows = query(b, &format!("SELECT COUNT(*) AS n FROM {table}"), &[]).await;
  ns(&rows)[0]
}

/// `table (n BIGINT)`, empty.
async fn create_numbers(b: &impl Backend, table: &str) {
  create(b, table, &[("n", SqlType::I64)]).await;
}

fn insert_n(tx: &Arc<dyn SqlTransaction>, sql: &str, n: i64) -> Effect<u64, SqlError> {
  tx.execute(sql, &params![n])
}

pub async fn execute_counts_affected_rows(b: &impl Backend, table: &str) {
  create(b, table, &[("n", SqlType::I64), ("label", SqlType::Text)]).await;
  let insert = insert_sql(b, table, &["n", "label"]);
  for n in 1..=3i64 {
    assert_eq!(execute(b, &insert, &params![n, format!("n{n}")]).await, 1);
  }
  let update = format!(
    "UPDATE {table} SET label = {} WHERE n >= {}",
    b.placeholder(1),
    b.placeholder(2)
  );
  assert_eq!(execute(b, &update, &params!["big", 2i64]).await, 2);
  let rows = query(b, &format!("SELECT n, label FROM {table} ORDER BY n"), &[]).await;
  let labels: Vec<String> = rows.iter().map(|r| r.get("label").unwrap()).collect();
  assert_eq!(labels, ["n1", "big", "big"]);
  assert_eq!(ns(&rows), [1, 2, 3]);
  assert_eq!(execute(b, &format!("DELETE FROM {table}"), &[]).await, 3);
  assert_eq!(count(b, table).await, 0);
  drop_table(b, table).await;
}

fn failed<A>(r: &Result<A, SqlError>) -> bool {
  matches!(r, Err(SqlError::QueryFailed { .. }))
}

pub async fn statement_errors_are_query_failures(b: &impl Backend, table: &str) {
  let client = b.client();
  for sql in [
    "SELEC 1".to_owned(),
    format!("SELECT n FROM {table}_missing"),
  ] {
    let queried = run(client.query(&sql, &[])).await;
    assert!(failed(&queried), "{sql}: {queried:?}");
    let executed = run(client.execute(&sql, &[])).await;
    assert!(failed(&executed), "{sql}: {executed:?}");
    let streamed = run(client.query_stream(&sql, &[]).run_collect()).await;
    assert!(failed(&streamed), "{sql}: {streamed:?}");
  }
}

pub async fn empty_text_and_bytes_are_not_null(b: &impl Backend, table: &str) {
  create(b, table, &[("s", SqlType::Text), ("raw", SqlType::Bytes)]).await;
  execute(
    b,
    &insert_sql(b, table, &["s", "raw"]),
    &params!["", Vec::<u8>::new()],
  )
  .await;
  let rows = query(b, &format!("SELECT s, raw FROM {table}"), &[]).await;
  assert_eq!(
    rows[0].values(),
    [SqlValue::Text(String::new()), SqlValue::Bytes(Vec::new())]
  );
  drop_table(b, table).await;
}

pub async fn transactions_commit_and_roll_back(b: &impl Backend, table: &str) {
  create_numbers(b, table).await;
  let insert = insert_sql(b, table, &["n"]);
  let client = b.client();
  let sql = insert.clone();
  run(with_transaction(Arc::clone(&client), move |tx| {
    insert_n(&tx, &sql, 1)
  }))
  .await
  .expect("committed");
  assert_eq!(count(b, table).await, 1);
  let sql = insert.clone();
  let err = run(with_transaction(Arc::clone(&client), move |tx| {
    insert_n(&tx, &sql, 2).then(fail::<(), _, _>(SqlError::Unsupported("abort".into())))
  }))
  .await
  .unwrap_err();
  assert_eq!(err, SqlError::Unsupported("abort".into()));
  assert_eq!(
    count(b, table).await,
    1,
    "a failed transaction is rolled back"
  );
  // A handle's statements see its own writes; a rollback discards them.
  let tx = run(client.begin()).await.expect("begin");
  run(insert_n(&tx, &insert, 3)).await.expect("insert");
  let inside = run(tx.query(&format!("SELECT COUNT(*) AS n FROM {table}"), &[]))
    .await
    .expect("count in the transaction");
  assert_eq!(ns(&inside), [2]);
  run(tx.rollback()).await.expect("rollback");
  assert_eq!(count(b, table).await, 1);
  // A finished handle refuses everything.
  let tx = run(client.begin()).await.expect("begin");
  run(tx.commit()).await.expect("commit");
  let finished = |r: Result<(), SqlError>| matches!(r, Err(SqlError::TransactionFailed(_)));
  assert!(finished(run(tx.commit()).await));
  assert!(finished(run(tx.rollback()).await));
  assert!(finished(
    run(tx.execute(&insert, &params![4i64]).map(|_| ())).await
  ));
  assert!(finished(run(tx.query("SELECT 1", &[]).map(|_| ())).await));
  drop_table(b, table).await;
}

pub async fn savepoint_rolls_back_inner_only(b: &impl Backend, table: &str) {
  create_numbers(b, table).await;
  let insert = insert_sql(b, table, &["n"]);
  let missing = format!("INSERT INTO {table}_missing VALUES (1)");
  let program = with_transaction(b.client(), move |tx| {
    let (a, c, d, e, f) = (
      insert.clone(),
      insert.clone(),
      insert.clone(),
      insert.clone(),
      insert.clone(),
    );
    // A typed failure, then a server error (pg aborts the transaction until ROLLBACK TO).
    let failed = savepoint(&tx, "line", move |sp| {
      insert_n(&sp, &a, 2).then(fail::<u64, _, _>(SqlError::Unsupported("bad line".into())))
    });
    let broken = savepoint(&tx, "broken", move |sp| {
      insert_n(&sp, &insert, 3).then(sp.execute(&missing, &[]))
    });
    let nested = savepoint(&tx, "outer", move |sp| {
      let inner = savepoint(&sp, "inner", move |i| {
        insert_n(&i, &c, 5).then(fail::<u64, _, _>(SqlError::Unsupported("inner".into())))
      });
      insert_n(&sp, &d, 4).then(inner.catch(|_| succeed(0)))
    });
    insert_n(&tx, &e, 1)
      .then(failed.catch(|_| succeed(0)))
      .then(broken.map(|_| None).catch(|e| succeed(Some(e))))
      .flat_map(move |broken| insert_n(&tx, &f, 6).then(nested).map(move |_| broken))
  });
  let broken = run(program).await.expect("the outer transaction commits");
  assert!(
    matches!(broken, Some(SqlError::QueryFailed { .. })),
    "{broken:?}"
  );
  let rows = query(b, &format!("SELECT n FROM {table} ORDER BY n"), &[]).await;
  assert_eq!(ns(&rows), [1, 4, 6]);
  // Names are checked before anything is sent.
  let tx = run(b.client().begin()).await.expect("begin");
  for bad in [
    tx.create_savepoint("a b"),
    tx.release_savepoint("1"),
    tx.rollback_to_savepoint("x;y"),
  ] {
    assert!(matches!(run(bad).await, Err(SqlError::Unsupported(_))));
  }
  run(tx.rollback()).await.expect("rollback");
  drop_table(b, table).await;
}

pub async fn every_isolation_level_begins_and_commits(b: &impl Backend, table: &str) {
  create_numbers(b, table).await;
  let levels = [
    Isolation::ReadUncommitted,
    Isolation::ReadCommitted,
    Isolation::RepeatableRead,
    Isolation::Serializable,
  ];
  for (n, level) in (0i64..).zip(levels) {
    let sql = insert_sql(b, table, &["n"]);
    let inserted = run(with_transaction_options(b.client(), level, move |tx| {
      insert_n(&tx, &sql, n)
    }))
    .await;
    assert_eq!(inserted, Ok(1), "{level:?}");
  }
  assert_eq!(count(b, table).await, 4);
  drop_table(b, table).await;
}

/// The numbers `1..=limit`, computed by the database (a recursive CTE every backend supports).
fn numbers_sql(b: &impl Backend) -> String {
  format!(
    "WITH RECURSIVE s(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM s WHERE n < {}) \
     SELECT n FROM s ORDER BY n",
    b.placeholder(1)
  )
}

pub async fn query_stream_yields_every_row_in_order(b: &impl Backend) {
  let client = b.client();
  let rows = run(
    client
      .query_stream(&numbers_sql(b), &params![1000i64])
      .run_collect(),
  )
  .await
  .expect("stream");
  assert_eq!(ns(&rows), (1..=1000).collect::<Vec<i64>>());
  let one = run(
    client
      .query_stream(&numbers_sql(b), &params![1i64])
      .run_collect(),
  )
  .await
  .expect("one row");
  assert_eq!(ns(&one), [1]);
  let empty = run(
    client
      .query_stream("SELECT 1 AS n WHERE 1 = 0", &[])
      .run_collect(),
  )
  .await
  .expect("empty");
  assert!(empty.is_empty());
}

pub async fn dropped_streams_release_their_connections(b: &impl Backend) {
  let client = b.client();
  for _ in 0..2 * b.max_connections() {
    // More rows than one chunk, so each stream is dropped mid-result; a leak holds its
    // connection whatever the size, and fewer rows keep MySQL's drain of the rest short.
    let (_, rest) = run(
      client
        .query_stream(&numbers_sql(b), &params![10_000i64])
        .next_chunk(),
    )
    .await
    .expect("first chunk")
    .expect("rows");
    drop(rest);
  }
  // Every pooled connection would be held by now if a dropped stream kept its own. A released
  // one can still be busy: MySQL hands it back only after the server has sent the rest of the
  // result, which on a slow runner outlasts one pool acquire. So the
  // query may wait for the drains, bounded; a leaked connection never comes back.
  let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
  let rows = loop {
    match run(client.query("SELECT 1 AS n", &[])).await {
      Ok(rows) => break rows,
      Err(SqlError::NotConnected) if std::time::Instant::now() < deadline => {}
      Err(e) => panic!("SELECT 1 AS n after the dropped streams: {e}"),
    }
  };
  assert_eq!(ns(&rows), [1]);
}

/// One `#[tokio::test]` per conformance body, for the backend `$make` builds: `$make(name)` is
/// an `async fn(&str) -> Option<B: Backend>` (`None` skips the test, e.g. no database). Table
/// names start with `$prefix`.
#[allow(unused_macros)]
macro_rules! conformance_suite {
  ($prefix:literal, $make:path) => {
    #[tokio::test]
    async fn typed_binds_round_trip_uuid_timestamp_decimal_json() {
      let t = concat!($prefix, "_typed");
      let Some(b) = $make(t).await else { return };
      $crate::conformance::typed_binds_round_trip_uuid_timestamp_decimal_json(&b, t).await;
    }

    #[tokio::test]
    async fn row_get_by_name_and_index() {
      let Some(b) = $make(concat!($prefix, "_row_get")).await else {
        return;
      };
      $crate::conformance::row_get_by_name_and_index(&b).await;
    }

    #[tokio::test]
    async fn untyped_null_binds_into_int_text_uuid_columns() {
      let t = concat!($prefix, "_null");
      let Some(b) = $make(t).await else { return };
      $crate::conformance::untyped_null_binds_into_int_text_uuid_columns(&b, t).await;
    }

    #[tokio::test]
    async fn execute_counts_affected_rows() {
      let t = concat!($prefix, "_affected");
      let Some(b) = $make(t).await else { return };
      $crate::conformance::execute_counts_affected_rows(&b, t).await;
    }

    #[tokio::test]
    async fn statement_errors_are_query_failures() {
      let t = concat!($prefix, "_errors");
      let Some(b) = $make(t).await else { return };
      $crate::conformance::statement_errors_are_query_failures(&b, t).await;
    }

    #[tokio::test]
    async fn empty_text_and_bytes_are_not_null() {
      let t = concat!($prefix, "_empty");
      let Some(b) = $make(t).await else { return };
      $crate::conformance::empty_text_and_bytes_are_not_null(&b, t).await;
    }

    #[tokio::test]
    async fn transactions_commit_and_roll_back() {
      let t = concat!($prefix, "_tx");
      let Some(b) = $make(t).await else { return };
      $crate::conformance::transactions_commit_and_roll_back(&b, t).await;
    }

    #[tokio::test]
    async fn savepoint_rolls_back_inner_only() {
      let t = concat!($prefix, "_savepoints");
      let Some(b) = $make(t).await else { return };
      $crate::conformance::savepoint_rolls_back_inner_only(&b, t).await;
    }

    #[tokio::test]
    async fn every_isolation_level_begins_and_commits() {
      let t = concat!($prefix, "_isolation");
      let Some(b) = $make(t).await else { return };
      $crate::conformance::every_isolation_level_begins_and_commits(&b, t).await;
    }

    #[tokio::test]
    async fn query_stream_yields_every_row_in_order() {
      let Some(b) = $make(concat!($prefix, "_stream")).await else {
        return;
      };
      $crate::conformance::query_stream_yields_every_row_in_order(&b).await;
    }

    #[tokio::test]
    async fn dropped_streams_release_their_connections() {
      let Some(b) = $make(concat!($prefix, "_stream_drop")).await else {
        return;
      };
      $crate::conformance::dropped_streams_release_their_connections(&b).await;
    }

    #[cfg(feature = "proptest")]
    #[test]
    fn prop_values_round_trip_through_insert_select() {
      use id_effect_sql::testing::arb_sql_value;
      use proptest::test_runner::{Config, TestCaseError, TestRunner};

      let t = concat!($prefix, "_prop");
      let rt = tokio::runtime::Runtime::new().expect("tokio");
      let Some(b) = rt.block_on($make(t)) else {
        return;
      };
      rt.block_on($crate::conformance::create_value_table(&b, t));
      let mut runner = TestRunner::new(Config {
        cases: 64,
        failure_persistence: None,
        ..Config::default()
      });
      let outcome = runner.run(&arb_sql_value(), |value| {
        rt.block_on($crate::conformance::value_round_trips(&b, t, value))
          .map_err(TestCaseError::fail)
      });
      rt.block_on($crate::conformance::drop_table(&b, t));
      outcome.expect("every generated value round-trips");
    }
  };
}
#[allow(unused_imports)]
pub(crate) use conformance_suite;
