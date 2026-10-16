//! Driver conformance bodies: the same typed round trips for every backend, written against
//! `Arc<dyn SqlClient>` and a [`Backend`] that knows its DDL and placeholders. `tests/pg.rs`
//! instantiates them for PostgreSQL; SQLite and MySQL (E3.1b) only add a `Backend` impl.
//!
//! Each body takes a table name owned by the calling test (tests run concurrently).

use std::sync::Arc;

use id_effect::{Cause, Effect, Exit};
use id_effect_sql::jiff::civil::{Date, DateTime, Time, date, time};
use id_effect_sql::rust_decimal::Decimal;
use id_effect_sql::serde_json::{self, json};
use id_effect_sql::uuid::Uuid;
use id_effect_sql::{Json, SqlClient, SqlError, SqlRow, SqlType, SqlValue, params};
use id_effect_tokio::TokioRuntime;

/// One database under test.
pub trait Backend {
  /// A connected client.
  fn client(&self) -> Arc<dyn SqlClient>;
  /// The column type a value of `t` is stored in.
  fn column_ddl(&self, t: SqlType) -> &'static str;
  /// The `n`-th (1-based) parameter placeholder.
  fn placeholder(&self, n: usize) -> String;
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
  let declared: Vec<SqlType> = columns.iter().map(|(_, t)| *t).collect();
  assert_eq!(types, declared);
  drop_table(b, table).await;
}

pub async fn row_get_by_name_and_index(b: &impl Backend) {
  let rows = query(b, "SELECT CAST(1 AS BIGINT) AS a, 'x' AS b, NULL AS c", &[]).await;
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

#[allow(deprecated)]
pub async fn deprecated_sqlparam_still_binds(b: &impl Backend, table: &str) {
  use id_effect_sql::SqlParam;
  let columns = [
    ("n", SqlType::I64),
    ("s", SqlType::Text),
    ("nil", SqlType::Text),
    ("ok", SqlType::Bool),
    ("raw", SqlType::Bytes),
  ];
  create(b, table, &columns).await;
  let legacy = [
    SqlParam::Int(5),
    SqlParam::Text("five".into()),
    SqlParam::Null,
    SqlParam::Bool(false),
    SqlParam::Bytes(vec![5]),
  ];
  let bound: Vec<SqlValue> = legacy.iter().cloned().map(SqlValue::from).collect();
  execute(
    b,
    &insert_sql(b, table, &["n", "s", "nil", "ok", "raw"]),
    &bound,
  )
  .await;
  let rows = query(b, &format!("SELECT n, s, nil, ok, raw FROM {table}"), &[]).await;
  assert_eq!(rows[0].values(), legacy);
  assert_eq!(rows[0].text_cells(), ["5", "five", "", "false", "[5]"]);
  drop_table(b, table).await;
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
/// and compares.
pub async fn value_round_trips(
  b: &impl Backend,
  table: &str,
  value: SqlValue,
) -> Result<(), String> {
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
  if *back == value {
    Ok(())
  } else {
    Err(format!("{value:?} came back as {back:?}"))
  }
}
