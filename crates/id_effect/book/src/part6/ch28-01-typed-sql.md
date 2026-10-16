# Typed SQL values and rows

Since 0.9, `id_effect_sql` binds and returns **typed values**. A parameter is a [`SqlValue`](https://docs.rs/id_effect_sql/latest/id_effect_sql/enum.SqlValue.html), a result row is a [`SqlRow`](https://docs.rs/id_effect_sql/latest/id_effect_sql/struct.SqlRow.html) of named columns, and `row.get::<T>(column)` decodes one column into a Rust type, returning a `SqlError` that names the column instead of panicking. The same API serves every driver and the in-memory `TestSqlClient`.

## Parameters: `SqlValue` and `params!`

`SqlClient::query` / `execute` (and the free `query` / `execute`, and `SqlTransaction`) take `&[SqlValue]`. Build the slice with the `params!` macro, which encodes each argument by reference with [`ToSqlValue`](https://docs.rs/id_effect_sql/latest/id_effect_sql/trait.ToSqlValue.html), or with `From` / `.into()`:

```rust
use id_effect_sql::{SqlValue, params};

let (id, title) = (7i64, String::from("write docs"));
let by_macro = params![id, title, true, None::<String>]; // `title` is borrowed, not moved
let by_hand: Vec<SqlValue> = vec![id.into(), title.as_str().into(), SqlValue::Bool(true), SqlValue::Null];
assert_eq!(by_macro, by_hand);
assert!(params![].is_empty());
```

`SqlValue` has one variant per portable type: `Null`, `Bool`, `I16`/`I32`/`I64`, `F32`/`F64`, `Text`, `Bytes`, `Uuid`, `Timestamp`, `DateTime`, `Date`, `Time`, `Decimal` and `Json`. It is `#[non_exhaustive]` (more types may come in a minor release) and `PartialEq` only, because floats have no total equality.

## Reading rows: `get::<T>`

A column is addressed by name (first match, case-sensitive) or by position. Every failure is a `SqlError` naming the column, never a panic, so decoding composes with `?` inside the effect that ran the query:

```rust
use id_effect_sql::{SqlError, SqlRow, SqlType, SqlValue};

let row = SqlRow::from_pairs([
    ("id", SqlValue::I64(7)),
    ("title", SqlValue::Text("write docs".into())),
    ("due", SqlValue::Null),
]);

assert_eq!(row.get::<i64>("id"), Ok(7));
assert_eq!(row.get::<i32>(0usize), Ok(7)); // integers convert, range-checked
assert_eq!(row.get::<String>("title").as_deref(), Ok("write docs"));

assert_eq!(row.get::<i64>("owner"), Err(SqlError::ColumnNotFound { column: "owner".into() }));
let err = row.get::<bool>("title").unwrap_err();
assert_eq!(
    err,
    SqlError::TypeMismatch { column: "title".into(), expected: SqlType::Bool, found: SqlType::Text }
);
assert_eq!(err.to_string(), "sql column `title`: expected Bool, found Text");

let wide = SqlRow::from_pairs([("n", i64::MAX)]);
assert!(matches!(wide.get::<i32>("n"), Err(SqlError::ValueOutOfRange { column, .. }) if column == "n"));
```

A typical row type decodes its fields one by one and maps `SqlError` into the domain's error with `From`:

```rust
use id_effect_sql::{SqlError, SqlRow};

#[derive(Debug, PartialEq)]
struct Todo { id: i64, title: String, completed: bool }

#[derive(Debug)]
enum StoreError { Corrupt(String) }
impl From<SqlError> for StoreError {
    fn from(e: SqlError) -> Self { StoreError::Corrupt(e.to_string()) }
}

fn todo(row: &SqlRow) -> Result<Todo, StoreError> {
    Ok(Todo { id: row.get("id")?, title: row.get("title")?, completed: row.get("completed")? })
}

let row = SqlRow::from_pairs([("id", 1i64)]);
let Err(StoreError::Corrupt(why)) = todo(&row) else { panic!("decoded a partial row") };
assert_eq!(why, "sql column `title` not found");
```

`SqlError` is `#[non_exhaustive]`: a `match` on it needs a `_ =>` arm. Besides `get`, `row.value(idx)` returns the raw `&SqlValue`, and `columns()` / `values()` expose the whole row (`SqlColumn::db_type()` is the driver's type name, e.g. `INT4`).

## NULL

`SqlValue::Null` is bound **untyped**: the database infers its type from the statement (`WHERE owner_id = $1`, `INSERT … VALUES ($1)`), so the same `Null` fits an integer, text or uuid column. `Option<T>` maps `None` to `Null` in both directions; decoding a NULL into a non-`Option` type is `SqlError::UnexpectedNull`:

```rust
use id_effect_sql::{SqlError, SqlRow, SqlValue, params};

assert_eq!(params![Some(3i32), None::<i64>], [SqlValue::I32(3), SqlValue::Null]);

let row = SqlRow::from_pairs([("due", SqlValue::Null)]);
assert_eq!(row.get::<Option<String>>("due"), Ok(None));
assert_eq!(row.get::<String>("due"), Err(SqlError::UnexpectedNull { column: "due".into() }));
```

## Dates, UUIDs, decimals and JSON

The value crates are re-exported (`id_effect_sql::{jiff, uuid, rust_decimal, serde_json}`), so your code uses the same versions:

| Rust type | `SqlValue` | PostgreSQL |
|---|---|---|
| `bool` | `Bool` | `BOOL` |
| `i16` / `i32` / `i64` (`u8` / `u16` / `u32` widen) | `I16` / `I32` / `I64` | `INT2` / `INT4` / `INT8` |
| `f32` / `f64` | `F32` / `F64` | `FLOAT4` / `FLOAT8` |
| `String`, `&str` | `Text` | `TEXT`, `VARCHAR`, enums |
| `Vec<u8>`, `&[u8]` | `Bytes` | `BYTEA` |
| `uuid::Uuid` | `Uuid` | `UUID` |
| `jiff::Timestamp` (an instant) | `Timestamp` | `TIMESTAMPTZ` |
| `jiff::civil::DateTime` (wall clock) | `DateTime` | `TIMESTAMP` |
| `jiff::civil::Date` / `Time` | `Date` / `Time` | `DATE` / `TIME` |
| `rust_decimal::Decimal` | `Decimal` | `NUMERIC` (≤ 28 digits) |
| `serde_json::Value`, `Json<T>` | `Json` | `JSON`, `JSONB` |

An instant and a wall-clock time are never converted into each other implicitly. Databases store microseconds, so nanoseconds are truncated. Decoding never parses text into numbers or booleans, but it does parse text into uuids, dates and times, decimals and JSON (SQLite stores those as text). A PostgreSQL column of another type (`INET`, `INTERVAL`, arrays) is a `SqlError::Unsupported` naming the column; cast it in SQL (`addr::text`).

```rust
use id_effect_sql::jiff::{Timestamp, civil};
use id_effect_sql::rust_decimal::Decimal;
use id_effect_sql::uuid::Uuid;
use id_effect_sql::{SqlRow, SqlValue, params};

let id = Uuid::nil();
let paid_at = Timestamp::from_second(1_700_000_000).unwrap();
let due = civil::date(2026, 3, 31);
let amount = Decimal::new(1999, 2); // 19.99
let p = params![id, paid_at, due, amount];
assert_eq!(p[3], SqlValue::Decimal(amount));

let row = SqlRow::from_pairs([("id", SqlValue::Text(id.to_string())), ("due", SqlValue::Date(due))]);
assert_eq!(row.get::<Uuid>("id"), Ok(id)); // parsed from text
assert_eq!(row.get::<civil::Date>("due"), Ok(due));
```

A JSON column carries a `serde_json::Value`. For a typed payload, encode with `SqlValue::json(&value)` (fallible: serde may refuse a value) and decode with `Json<T>`; a document of the wrong shape is a `SqlError::Decode` naming the column:

```rust
use id_effect_sql::{Json, SqlError, SqlRow, SqlValue};

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
struct Tags { names: Vec<String> }

let tags = Tags { names: vec!["urgent".into()] };
let row = SqlRow::from_pairs([("tags", SqlValue::json(&tags).unwrap())]);
let Json(back) = row.get::<Json<Tags>>("tags").unwrap();
assert_eq!(back, tags);

let bad = SqlRow::from_pairs([("tags", SqlValue::Text("[1, 2]".into()))]);
assert!(matches!(bad.get::<Json<Tags>>("tags"), Err(SqlError::Decode(m)) if m.contains("`tags`")));
```

## Your own types

Implement `ToSqlValue` and `FromSqlValue` by delegating to the wrapped value; validation failures become `ValueError::Invalid`, which `get` reports with the column:

```rust
use id_effect_sql::{FromSqlValue, SqlRow, SqlValue, ToSqlValue, ValueError, params};

#[derive(Debug, PartialEq)]
struct Email(String);

impl ToSqlValue for Email {
    fn to_sql_value(&self) -> SqlValue { self.0.to_sql_value() }
}
impl FromSqlValue for Email {
    fn from_sql_value(v: &SqlValue) -> Result<Self, ValueError> {
        let s = String::from_sql_value(v)?;
        if s.contains('@') { Ok(Email(s)) } else { Err(ValueError::Invalid(format!("not an email: {s}"))) }
    }
}

assert_eq!(params![Email("a@b.c".into())], [SqlValue::Text("a@b.c".into())]);
let row = SqlRow::from_pairs([("email", "nobody")]);
assert!(row.get::<Email>("email").unwrap_err().to_string().contains("column `email`"));
```

## Rows into structs: `FromRow`

`#[derive(FromRow)]` turns a row into a struct. Each field reads its column with
`row.get::<FieldTy>(column)`, so an `Option<T>` field accepts NULL and an error names the column.
The column is the field name, the `#[schema("…")]` rename if the type also derives `Schema`, or
`#[from_row(rename = "col")]`:

```rust
use id_effect_sql::{FromRow, SqlRow, SqlValue};

#[derive(Debug, PartialEq, FromRow)]
struct Todo {
    id: i64,
    #[from_row(rename = "text")]
    title: String,
    due: Option<String>,
}

let row = SqlRow::from_pairs([
    ("id", SqlValue::I64(7)),
    ("text", SqlValue::Text("ship 0.9".into())),
    ("due", SqlValue::Null),
]);
assert_eq!(
    Todo::from_row(&row).unwrap(),
    Todo { id: 7, title: "ship 0.9".into(), due: None }
);
```

`#[from_row(default)]` fills a missing column with `Default::default()`, and
`#[from_row(flatten)]` decodes a nested `FromRow` struct from the same row. Tuples decode by
position: `<(i64, String)>::from_row(&row)`. Put `#[from_row(schema)]` on a type with a `Schema`
and the whole row runs through that schema, so refinements and brands check database data too;
failures come back together as `SqlError::InvalidRow { issues }`. `query_as`, `query_one_as`
(`SqlError::RowNotFound` when no row comes back) and `query_opt_as` run a query and decode it in one
step.

## Savepoints, isolation and streaming

A transaction can open nested scopes with `savepoint(&tx, "line", |sp| …)`. If the inner work fails,
panics or is interrupted, only that work is rolled back (`ROLLBACK TO SAVEPOINT`); the outer
transaction stays usable. Savepoints nest, and their names are generated, never built from input.

`transaction_with(Isolation::Serializable, |tx| …)` or
`with_transaction_options(client, TransactionOptions::from(Isolation::Serializable).with_read_only(), f)`
runs `SET TRANSACTION …` first. Under `SERIALIZABLE` a transaction can lose a conflict; wrap it in
`retry_serialization_failures(Schedule::recurs(5), || transaction_with(…))` to rerun it on SQLSTATE
40001 or 40P01 only.

`query_stream(sql, params)` returns a `Stream<SqlRow, SqlError, R>`, and `query_stream_as::<T>`
decodes each row with `FromRow`. Rows arrive one chunk per pull, so a slow consumer slows the
database down instead of filling memory. If the stream is dropped or its fiber interrupted,
PostgreSQL cancels the query and the connection returns to the pool.

## Testing with `TestSqlClient`

Script result rows with `SqlRow::from_pairs` and assert on the typed parameters the code bound (`recorded_queries()` / `recorded_executes()` return `Vec<(String, Vec<SqlValue>)>`). No database is needed:

```rust
use id_effect::{Effect, Exit, fail, succeed};
use id_effect_sql::{SqlClientEnv, SqlError, SqlRow, SqlValue, TestSqlClient, params, query, sql_client_layer};

const OPEN: &str = "SELECT id, title FROM todos WHERE completed = $1 ORDER BY id";

fn open_titles() -> Effect<Vec<String>, SqlError, SqlClientEnv> {
    query(OPEN, params![false]).flat_map(|rows| {
        match rows.iter().map(|r| r.get::<String>("title")).collect::<Result<Vec<_>, _>>() {
            Ok(titles) => succeed(titles),
            Err(e) => fail(e),
        }
    })
}

let test = TestSqlClient::new();
test.script_query(OPEN, vec![
    SqlRow::from_pairs([("id", SqlValue::I64(1)), ("title", "a".into())]),
    SqlRow::from_pairs([("id", SqlValue::I64(2)), ("title", "b".into())]),
]);
let exit = open_titles().provide_layer(sql_client_layer::<SqlError>(test.clone())).block_on();
assert!(matches!(exit, Exit::Success(ref t) if t == &["a", "b"]));
assert_eq!(test.recorded_queries(), [(OPEN.to_owned(), vec![SqlValue::Bool(false)])]);
```

## Coming from 0.8

`SqlParam` (now `SqlValue::from(param)`), `SqlRow::new(cells)` (now `SqlRow::from_pairs`) and `SqlRow::text_cells()` still compile with a deprecation warning and are removed in 0.10; `row.cells` is gone. The full list of changes is in [`docs/MIGRATION-0.9.md`](https://github.com/Industrial/id_effect/blob/main/docs/MIGRATION-0.9.md) ("Data: SQL values and rows").
