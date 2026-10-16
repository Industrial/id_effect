---
slug: r09-e3-1a-values-rows-design
task: tsk-mvbdsp4f-wd3fai
status: approved 2026-10-16 (owner accepted the recommendation for all seven open questions)
inputs: .maestro/specs/releases/0.9.0.md (E3.1a/E3.1b/E3.3/E5.5, D2, D3, D4, D10)
---

# E3.1a design: `SqlValue`, `SqlRow`, encoding traits (review before any driver work)

Scope: the value and row types every 0.9 data leaf builds on. Nothing here is implemented yet.
Today's code: `SqlParam` (5 variants, `derive(Eq)`) `crates/id_effect_sql/src/client.rs:51-63`;
`SqlRow { pub cells: Vec<String> }` `client.rs:66-78`; `SqlClient` `client.rs:81-99`; free fns
`query`/`execute` take `Vec<SqlParam>` `client.rs:137-155`; `SqlTransaction` `transaction.rs:31-40`;
pg bind `pg/client.rs:99-113` (NULL is bound as `None::<i32>`, `:105`); pg decode tries
i32/i64/bool/f64/String/bytes and returns `""` for everything else, so uuid/timestamp/numeric columns
silently become empty strings today (`pg/client.rs:115-140`); `SqlError` is an exhaustive enum with
`Decode(String)` `error.rs:6-23`; the crate has no uuid/jiff/decimal/json deps (`Cargo.toml:21-28`).

## 1. `SqlValue`

```rust
// crates/id_effect_sql/src/value.rs
#[derive(Clone, Debug, PartialEq)]          // no Eq/Hash: F32/F64 (see table)
#[non_exhaustive]
pub enum SqlValue {
  Null,                                     // untyped (decision 1c)
  Bool(bool),
  I16(i16), I32(i32), I64(i64),
  F32(f32), F64(f64),
  Text(String),
  Bytes(Vec<u8>),
  Uuid(uuid::Uuid),
  Timestamp(jiff::Timestamp),               // instant: pg TIMESTAMPTZ, mysql TIMESTAMP
  DateTime(jiff::civil::DateTime),          // wall clock: pg TIMESTAMP, mysql DATETIME (Q3)
  Date(jiff::civil::Date),
  Time(jiff::civil::Time),                  // (Q3)
  Decimal(rust_decimal::Decimal),
  Json(serde_json::Value),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SqlType { Null, Bool, I16, I32, I64, F32, F64, Text, Bytes, Uuid,
                   Timestamp, DateTime, Date, Time, Decimal, Json }

impl SqlValue {
  pub fn sql_type(&self) -> SqlType;
  pub fn is_null(&self) -> bool;
  pub fn json<T: serde::Serialize>(value: &T) -> Result<Self, SqlError>; // Json(serde_json::to_value)
  pub fn to_unknown(&self) -> id_effect::schema::Unknown;                // for FromRow, section 3
}
```

| # | Decision | Why |
|---|---|---|
| 1a | `#[non_exhaustive]` on `SqlValue`, `SqlType`, and (new) `SqlError` | Arrays, `U64`, intervals, typed nulls can be added in a minor without another break. |
| 1b | `PartialEq` only; no `Eq`/`Hash` | F32/F64 (NaN). `Decimal`, `Uuid`, jiff and `serde_json::Value` are all `Eq`, so floats are the only blocker; a total-order wrapper is not worth it. The spec flags `derive(Eq)` at `client.rs:51` as the reason `f64` was impossible. sea-query's `Value` is `PartialEq`-only too. |
| 1c | `Null` is a unit variant bound with *unspecified* type (pg OID 0); `Option::<T>::None` encodes to `Null` | The server infers the type from context (`WHERE uuid_col = $1`, `INSERT … VALUES ($1)`), unlike today's `None::<i32>` (`pg/client.rs:105`), which fails on non-int columns. Bare `SELECT $1` infers `text`. If OID 0 misbehaves, a `NullOf(SqlType)` variant is additive (Q2). |
| 1d | jiff public, `time` only inside drivers (D4) | `Timestamp` = absolute instant, `DateTime` = civil wall clock; never convert one into the other implicitly. Sub-microsecond nanos are truncated on pg/mysql (documented). |
| 1e | `Json(serde_json::Value)` unboxed | Approved in D4; pg JSON and JSONB both decode to it. Typed payloads go through `SqlValue::json(&t)` / `FromSqlValue for Json<T>`. |
| 1f | Arrays **out** of 0.9 | pg-only (SQLite/MySQL have none, so they would break E3.1b's "one portable API" check #6); sea-query `Array` maps to `SqlError::Unsupported`. Add `Array(Vec<SqlValue>)` later as non-exhaustive growth. |

## 2. Encoding traits and errors

Names: `ToSqlValue` / `FromSqlValue`. `ToSql`/`FromSql` collide in readers' heads with
tokio-postgres/diesel/rusqlite traits of the same name and different shape (Q6).

```rust
pub trait ToSqlValue { fn to_sql_value(&self) -> SqlValue; }              // by ref: entities encode fields without moving
pub trait FromSqlValue: Sized { fn from_sql_value(v: &SqlValue) -> Result<Self, ValueError>; }

#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ValueError {                     // column-free; SqlRow::get adds the column
  UnexpectedNull,
  Mismatch { expected: SqlType, found: SqlType },
  OutOfRange(String),                     // i64 → i32 overflow, NUMERIC beyond 28 digits, year > 9999
  Invalid(String),                        // text fallback that does not parse, bad JSON for Json<T>
}

impl<T: ToSqlValue + ?Sized> ToSqlValue for &T { … }
impl<T: ToSqlValue> ToSqlValue for Option<T> { /* None → Null */ }
impl<T: FromSqlValue> FromSqlValue for Option<T> { /* Null → None */ }
impl<T: Into<SqlValue>> From<Option<T>> for SqlValue { … }               // so `x.into()` works for Option too
#[macro_export] macro_rules! params { ($($v:expr),* $(,)?) =>
  { ::std::vec![$($crate::ToSqlValue::to_sql_value(&$v)),*] } }      // params![id, title, done]
```

| Rust type | `ToSqlValue` / `From<T> for SqlValue` | `FromSqlValue` accepts |
|---|---|---|
| `bool` | `Bool` | `Bool` |
| `i16` / `i32` / `i64` | `I16` / `I32` / `I64` | any integer variant, range-checked (`OutOfRange`) — SQLite returns everything as `I64`, pg `count(*)` is `INT8` |
| `u8`, `u16`, `u32` | `I16`, `I32`, `I64` | integer variants, range-checked |
| `u64` | — (cannot be infallible) | integer variants ≥ 0 |
| `f32` / `f64` | `F32` / `F64` | `F32`, `F64` (`f32` from `F64` only if exactly representable); no int→float |
| `String`, `&str`, `Box<str>`, `Arc<str>` | `Text` | `Text` |
| `Vec<u8>`, `&[u8]` | `Bytes` | `Bytes` |
| `uuid::Uuid` | `Uuid` | `Uuid`; `Bytes` of len 16; `Text` that parses |
| `jiff::Timestamp` | `Timestamp` | `Timestamp`; `Text` RFC 3339 with offset |
| `jiff::civil::{DateTime, Date, Time}` | same-name variant | same-name variant; `Text` ISO 8601 |
| `rust_decimal::Decimal` | `Decimal` | `Decimal`; `I16/I32/I64`; `Text` that parses |
| `serde_json::Value` | `Json` | `Json`; `Text` that parses |
| `Json<T>` (`pub struct Json<T>(pub T)`) | via `SqlValue::json` (fallible) | `Json` / `Text` → `serde_json::from_value` (`Invalid`) |
| `SqlValue` | identity | identity |

Rule behind the "accepts" column: decoding never parses text into numbers or bools, but does parse
text into uuid/temporal/decimal/json, because SQLite stores those as TEXT and computed MySQL/SQLite
expressions lose the declared type (Q5). User types implement the two traits by delegation:

```rust
struct Email(String);
impl ToSqlValue for Email { fn to_sql_value(&self) -> SqlValue { self.0.to_sql_value() } }
impl FromSqlValue for Email {
  fn from_sql_value(v: &SqlValue) -> Result<Self, ValueError> {
    String::from_sql_value(v).and_then(|s| Email::parse(&s).map_err(|e| ValueError::Invalid(e.to_string())))
  }
}
```

`SqlError` (`error.rs:6-23`) becomes `#[non_exhaustive]` and gains variants; existing variants keep
their shape, so `SqlError::Decode(..)` / `QueryFailed{..}` literals still compile:

```rust
ColumnNotFound { column: String },
UnexpectedNull { column: String },
TypeMismatch   { column: String, expected: SqlType, found: SqlType },
ValueOutOfRange{ column: String, message: String },
// ValueError::Invalid(m) → Decode(format!("column `{column}`: {m}"))
// column = name, or "#3" for an index into an unnamed column
```
`StaleVersion` (E3.3) and row-validation issues (from-row leaf) are added by their leaves.

## 3. `SqlRow`

```rust
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SqlColumn { name: Arc<str>, sql_type: SqlType, db_type: Arc<str> } // db_type: "INT4", "jsonb", "DATETIME"
impl SqlColumn { pub fn new(name: impl Into<Arc<str>>, sql_type: SqlType) -> Self;
                 pub fn name(&self) -> &str; pub fn sql_type(&self) -> SqlType; pub fn db_type(&self) -> &str; }

#[derive(Clone, Debug, PartialEq, Default)]   // Default kept: metrics.rs tests use SqlRow::default()
pub struct SqlRow { columns: Arc<[SqlColumn]>, values: Vec<SqlValue> } // columns shared per result set

pub trait ColumnIndex: sealed::Sealed { fn position(&self, row: &SqlRow) -> Result<usize, SqlError>; }
// impls: usize, &str, &String. Name lookup: first match, case-sensitive (sqlx semantics).

impl SqlRow {
  pub fn from_parts(columns: Arc<[SqlColumn]>, values: Vec<SqlValue>) -> Result<Self, SqlError>; // len check
  pub fn from_pairs<I, N, V>(pairs: I) -> Self
    where I: IntoIterator<Item = (N, V)>, N: Into<Arc<str>>, V: ToSqlValue;           // tests / TestSqlClient
  pub fn columns(&self) -> &[SqlColumn];
  pub fn values(&self) -> &[SqlValue];
  pub fn into_values(self) -> Vec<SqlValue>;
  pub fn len(&self) -> usize;  pub fn is_empty(&self) -> bool;
  pub fn value(&self, idx: impl ColumnIndex) -> Result<&SqlValue, SqlError>;
  pub fn get<T: FromSqlValue>(&self, idx: impl ColumnIndex) -> Result<T, SqlError>;   // row.get::<i64>("id")
  pub fn to_unknown(&self) -> id_effect::schema::Unknown;                              // Object keyed by column name

  #[deprecated(since = "0.9.0", note = "build rows with SqlRow::from_pairs")]
  pub fn new(cells: Vec<String>) -> Self;            // columns "#0".."#n", SqlType::Text, values Text
  #[deprecated(since = "0.9.0", note = "use SqlRow::get::<T>(idx) or value(idx)")]
  pub fn text_cells(&self) -> Vec<String>;           // 0.8 rendering, see below
}
```

- `get` returns `Result` and never panics; there is **no** `try_get` (it would only duplicate `get`;
  absent-column-as-`None` is `row.value(i).ok()`). A panicking `get` would be a defect inside an
  `Effect`; sqlx's `get`-panics convention is the reason not to copy its names (Q7).
- Explicit turbofish works with `impl ColumnIndex` (stable since 1.63; MSRV 1.94 per D10).
- `text_cells()` renders exactly what 0.8 produced for the five legacy kinds (`Null` → `""`, ints and
  floats `to_string`, `Bytes` → `format!("{b:?}")`, `pg/client.rs:115-140`) and, beyond that, uuid
  hyphenated, jiff `Display`, decimal `to_string`, JSON compact (in 0.8 these came back as `""`).
- FromRow hook (owned by `r09-e3-1a-from-row`; listed here so this leaf exports what it needs): plain
  `#[derive(FromRow)]` = one `row.get::<FieldTy>(column)` per field (column = `#[schema("x")]` rename
  or field name). With `#[from_row(schema)]` on a type that also derives `Schema`, decode is
  `T::schema().decode_unknown_all(&row.to_unknown())` (`HasSchema` `crates/id_effect/src/schema/has_schema.rs`,
  accumulating decoder spec ref `parse.rs:492`; derived schemas take `Unknown::Object`,
  `crates/id_effect_proc_macro/src/lib.rs:189-201`), so refinements and brands run on DB data and
  every failing column is reported at once. `to_unknown`: `Null`→`Null`, `Bool`, ints→`I64`, floats→`F64`,
  `Text`/uuid/temporal (RFC 3339 / ISO)/decimal→`String`, `Bytes`→`Array` of `I64`, `Json`→tree.

Additive helpers in the same leaf: `SqlParam` → `SqlValue` conversion and a test-compat comparison.

```rust
#[deprecated(since = "0.9.0", note = "use SqlValue (SqlParam::Int → SqlValue::I64)")]
pub enum SqlParam { Null, Bool(bool), Int(i64), Text(String), Bytes(Vec<u8>) } // derives unchanged
#[allow(deprecated)] impl From<SqlParam> for SqlValue { /* Int → I64, others same name */ }
#[allow(deprecated)] impl PartialEq<SqlParam> for SqlValue { /* via From */ }  // keeps `assert_eq!(params, [SqlParam::Int(1)])` compiling
```

Signature changes (all `&[SqlParam]` → `&[SqlValue]`, `Vec<SqlParam>` → `Vec<SqlValue>`):
`SqlClient::query/execute` (`client.rs:86,89`), `SqlTransaction::query/execute` (`transaction.rs:33,35`),
free `query`/`execute` (`client.rs:137-155`), `TestSqlClient::recorded_queries/recorded_executes`
(return `Vec<(String, Vec<SqlValue>)>`, `client.rs:257-269`), `InstrumentedSqlClient` impl
(`metrics.rs:121-127`). `connect/begin/close` are unchanged. New export list in `lib.rs:58-62`:
`SqlValue, SqlType, SqlColumn, ColumnIndex, ToSqlValue, FromSqlValue, ValueError, Json, params!`,
plus `pub use {jiff, uuid, rust_decimal, serde_json};` so users match versions.

### Deprecation path (source for `docs/MIGRATION-0.9.md`)

| User code (0.8) | 0.9 | Fix |
|---|---|---|
| `client.query(sql, &[])`, `query(sql, vec![])`, `execute(..)`, transactions, `SqlRow::default()`, `rows.len()` | compiles, no warning | — |
| `match err { SqlError::NotConnected => .., other => .. }` | compiles | — |
| `SqlParam::Int(1)` named anywhere; `SqlValue::from(p)` / `p.into()` | compiles, `deprecated` warning | `SqlValue::I64(1)` or `1i64.into()` |
| `SqlRow::new(vec!["1".into()])` (scripted test rows) | compiles, warning | `SqlRow::from_pairs([("id", 1i64)])` |
| `assert_eq!(recorded_params, [SqlParam::Int(1)])` | compiles (`PartialEq<SqlParam>`), warning | compare with `SqlValue` |
| `row.text_cells()` | new, warning | `row.get::<T>(idx)` |
| `client.query(sql, &[SqlParam::Int(1)])` / `query(sql, vec![SqlParam::..])` | **E0308** | `&[SqlValue::I64(1)]`, `&[x.into()]`, or `&params![x]` |
| `row.cells` / `SqlRow { cells }` | **E0616/E0451** (field private) | `row.get::<T>(i)`; transitional `row.text_cells()` |
| `impl SqlClient for X` / `impl SqlTransaction for X` | **E0053** | change the two signatures to `&[SqlValue]` |
| exhaustive `match` on `SqlError` | **E0004** | add `_ =>` |
| `T: Eq/Hash` bound satisfied by `SqlRow`/param vectors | **E0277** | use `PartialEq` |
| `let v: Vec<(String, Vec<SqlParam>)> = test.recorded_queries()` | **E0308** | `Vec<SqlValue>` |

The shim survives exactly one minor: `SqlParam`, `SqlRow::new`, `text_cells`, `PartialEq<SqlParam>`
are removed in 0.10 (D2 / L9). In-repo code migrates fully now (workspace clippy runs `-D warnings`).

## 4. Driver mapping

Feature layout (`crates/id_effect_sql/Cargo.toml`):

```toml
[features]
pg     = ["dep:sqlx", "dep:tokio", "dep:time", "sqlx/postgres"]
sqlite = ["dep:sqlx", "dep:tokio", "dep:time", "sqlx/sqlite"]   # E3.1b
mysql  = ["dep:sqlx", "dep:tokio", "dep:time", "sqlx/mysql"]    # E3.1b
proptest = ["dep:proptest", "id_effect/proptest"]                # exports testing::arb_sql_value
[dependencies]            # unconditional (Q1)
jiff = "0.2.23"           # same floor as crates/id_effect/Cargo.toml:75
uuid = { version = "1", default-features = false, features = ["std"] }
rust_decimal = { version = "1", default-features = false, features = ["std"] }
serde = "1"; serde_json = "1"
time = { version = "0.3", optional = true, default-features = false, features = ["std"] }
sqlx = { version = "0.8", optional = true, default-features = false,
         features = ["runtime-tokio", "uuid", "time", "rust_decimal", "json"] }  # "postgres" moves into `pg`
```
Every new dep and its MSRV is checked with `cargo +1.94 check`; anything newer is pinned with a
comment (D10). jiff↔time conversion lives in `src/value/time_conv.rs` (`pub(crate)`, compiled when any
driver feature is on): `Timestamp`↔`OffsetDateTime` via unix nanos (i128), `civil::DateTime`↔
`PrimitiveDateTime`, `Date` (`from_calendar_date`), `Time` (`from_hms_nano`); out-of-jiff-range
values (pg allows year 294276, `infinity`) → `ValueOutOfRange`. `value.rs` therefore becomes
`value/{mod.rs, encode.rs, time_conv.rs}` — a small expansion of the leaf's owned paths.

| `SqlValue` | pg bind (sqlx 0.8) | pg decode from `PgTypeInfo` | MySQL 8 (wave 2) | SQLite (wave 2) |
|---|---|---|---|---|
| `Null` | local `UntypedNull` with `PgTypeInfo::with_oid(Oid(0))`, `IsNull::Yes` | any NULL → `Null` | `None::<i64>` (MySQL NULL is untyped) | `None::<i64>` |
| `Bool` | `bool` | `BOOL` | `BOOLEAN`/`TINYINT(1)` → `Bool` | declared `BOOLEAN` → `Bool`, else `I64` |
| `I16/I32/I64` | `i16/i32/i64` | `INT2/INT4/INT8` (`OID` → `I64`) | `SMALLINT/INT/BIGINT`; `TINYINT`/`YEAR`→`I16`; unsigned fitting → `I64`, else `ValueOutOfRange` | INTEGER → `I64` |
| `F32/F64` | `f32/f64` | `FLOAT4/FLOAT8` | `FLOAT/DOUBLE` | REAL → `F64` |
| `Text` | `&str` | `TEXT, VARCHAR, BPCHAR, NAME`, enum types (`PgTypeKind::Enum`) | `CHAR/VARCHAR/TEXT/ENUM/SET` | TEXT |
| `Bytes` | `&[u8]` | `BYTEA` | `BINARY/VARBINARY/BLOB` | BLOB |
| `Uuid` | `Uuid` | `UUID` | bound as 16 bytes; read back as `Bytes`, decoded by `FromSqlValue` | declared `UUID` + 16-byte blob/text → `Uuid` |
| `Timestamp` | `OffsetDateTime` (UTC) | `TIMESTAMPTZ` | `TIMESTAMP` (session tz forced to `+00:00` at connect) | declared `TIMESTAMP*` → parse; bound as fixed-width `%Y-%m-%dT%H:%M:%S%.6fZ` text |
| `DateTime` | `PrimitiveDateTime` | `TIMESTAMP` | `DATETIME` (tests use `DATETIME(6)`) | declared `DATETIME` → parse; bound as text |
| `Date` / `Time` | `time::Date` / `time::Time` | `DATE` / `TIME` | `DATE` / `TIME` (negative or ≥24h → `ValueOutOfRange`) | declared `DATE`/`TIME`; text |
| `Decimal` | `Decimal` | `NUMERIC` (NaN or >28 digits → `ValueOutOfRange`) | `DECIMAL` | declared `DECIMAL/NUMERIC` → text kept exact |
| `Json` | `sqlx::types::Json(&Value)` (JSONB) | `JSON`, `JSONB` | `JSON` | declared `JSON` → parse text |
| other column types | — | `SqlError::Unsupported("column `x` has type INET; cast it (`x::text`)")` (Q4) | same | storage class decides |

pg decode switches on the column's type once per column (replacing the try-each cascade at
`pg/client.rs:115-140`) and builds the shared `Arc<[SqlColumn]>` once per result set. Bind/decode move
from `pg/client.rs:99-140` into a new `pg/value.rs`, reused by `pg/transaction.rs:9`.

**sea-query bridge (D3, E3.3, no binder).** Features `with-json, with-uuid, with-rust_decimal, with-time`
(chrono/bigdecimal/array/vector/ipnetwork off). `fn from_sea(v: sea_query::Value) -> Result<SqlValue, SqlError>`:

| `sea_query::Value` | `SqlValue` |
|---|---|
| any `X(None)` | `Null` |
| `Bool`, `TinyInt(i8)`/`SmallInt`, `Int`, `BigInt` | `Bool`, `I16`, `I32`, `I64` |
| `TinyUnsigned(u8)`, `SmallUnsigned(u16)`, `Unsigned(u32)`, `BigUnsigned(u64)` | `I16`, `I32`, `I64`, `I64` if ≤ `i64::MAX` else `ValueOutOfRange` |
| `Float`, `Double`, `String`, `Char`, `Bytes` | `F32`, `F64`, `Text`, `Text`, `Bytes` |
| `Json`, `Uuid`, `Decimal` | `Json`, `Uuid`, `Decimal` |
| `TimeDateTimeWithTimeZone`, `TimeDateTime`, `TimeDate`, `TimeTime` | `Timestamp`, `DateTime`, `Date`, `Time` (via `time_conv`) |
| anything else | `SqlError::Unsupported` |

Reverse `impl From<SqlValue> for sea_query::Value` (orphan-legal: `SqlValue` is local) lets users put
jiff values into builders without sea-query's own jiff support. sea-query renders `$n` for pg and `?`
for MySQL/SQLite — the placeholders sqlx expects, so the rendered SQL goes straight to `SqlClient::query`.

## 5. Test strategy

| Level | Where | What |
|---|---|---|
| Unit (no DB) | `src/value/*.rs`, `src/row.rs` `#[cfg(test)]` | every row of the section 2 table both ways; `Option`; range errors name the column (`decode_error_names_the_column`); `From<SqlParam>` + `PartialEq<SqlParam>`; golden `text_cells()` equal to 0.8 strings for the legacy kinds; `get` by name == by index; `ColumnNotFound`. |
| Property | `src/value/*.rs` tests + `id_effect_sql::testing::arb_sql_value()` (feature `proptest`, built on `id_effect::testing::property`) | `T::from_sql_value(&t.to_sql_value()) == t` for every std impl; `to_unknown` total; generators restricted to the portable domain: finite floats, text without `\0`, timestamps in years 1..=9999 truncated to µs, decimals ≤ 28 digits, JSON numbers in i32 range. |
| Driver round trip (pg, this leaf) | `tests/pg.rs` live tests (skip-or-fail protocol, `ID_EFFECT_REQUIRE_PG`) | spec tests `typed_binds_round_trip_uuid_timestamp_decimal_json`, `row_get_by_name_and_index`, `deprecated_sqlparam_still_binds`; plus `untyped_null_binds_into_int_text_uuid_columns` (proves 1c), `unsupported_column_type_is_a_clear_error`, `prop_values_round_trip_through_insert_select` (64 cases: insert each generated value into a column of its type, read back, compare). |
| Conformance (E3.1b) | `tests/conformance/mod.rs` | the same pg round-trip bodies written against `Arc<dyn SqlClient>` + a `Backend { client(), column_ddl(SqlType), placeholder(n) }` trait; this leaf writes the pg instantiation so SQLite/MySQL only add a backend impl. |

## 6. Ripple list (verified with `rg SqlParam|SqlRow|\.cells` outside the crate)

| File:line | Breaks? | Planned fix |
|---|---|---|
| `crates/id_effect_sql/src/client.rs:51-78,86,89,137-155,167-169,257-269,314-337` + tests `:398,:451-462` | yes (owner of the change) | as section 3; `TestSqlClient` stores `Vec<SqlValue>`; add `script_query` docs for `from_pairs` rows |
| `crates/id_effect_sql/src/transaction.rs:24,33,35,179-180,247-273` | yes | signatures + `TestSqlTransaction` pending vectors |
| `crates/id_effect_sql/src/metrics.rs:37,121-127` | yes | signatures |
| `crates/id_effect_sql/src/pg/client.rs:15,99-140,148,159`, `pg/transaction.rs:9,11,40,53` | yes | `pg/value.rs` bind/decode |
| `crates/id_effect_sql/src/lib.rs:18-44` (doc example), `:58-62`; `README.md:13,19,33,53`; `examples/010_test_client_transaction.rs:9,18,30` | warnings → `-D warnings` fails | rewrite on `SqlValue` / `from_pairs` / `get` |
| `crates/id_effect_sql/tests/pg.rs:21,99,193,196,205-218,252,375` | yes (`.cells`) | `rows[0].get::<i32>(0)`; `SqlValue` params |
| `examples/http-showcase/src/infrastructure/postgres/pg_todo_repository.rs:6,60-61,90,127-129,155` | yes (E0308) | `SqlValue::Text/I64/Bool`; tests `:181,:202,:253-255` → `from_pairs`, `SqlValue` |
| `…/postgres/migration.rs:6,28` | yes | `SqlValue::I64(Self::LOCK_KEY)` |
| `…/postgres/todo_row.rs:17-31` (`row.cells`), tests `:64-65` | yes | `TodoRow::parse` → `row.get::<i64>("id")`, `get::<String>("title")`, `get::<bool>("completed")`; fixtures via `from_pairs` (string "true"/"false" parsing disappears) |
| `…/postgres/pg_todo_repository_live.rs:69` | warning | `from_pairs` |
| `…/postgres/store_error_from_sql.rs:12-19`, `unique_violation.rs:17-19` | no (wildcard arm, existing variants) | — |
| `crates/id_effect/book/src/part6/ch28-00-data.md:19,29` | yes (doctest, E0308) | `SqlValue::Text(title)` |
| `crates/id_effect_tool/templates/service/src/db.rs.template:26` | no (`&[]` infers) | — |
| `crates/id_effect_node` (`pg`: `singleton.rs` uses only `PgPoolConfig` + raw sqlx) | no | — |
| `crates/id_effect_kit` (re-exports the crate, names no params/rows) | no | — |
| `labs/id_effect_{events,workflow,jobs}` (raw sqlx; `id_effect_sql` only for `pg` pool types) | no | build check only; note jobs enables sqlx `chrono`, which unifies harmlessly with our `time` |

The 0.9.0 spec's ripple list (node, kit, template, labs) over-states the break: only http-showcase,
the book chapter and the crate itself change. `r09-e3-1a-ripple-docs` keeps the build checks for the
others and shrinks to docs + showcase.

## 7. Open questions for the owner

| # | Question | Options | Recommendation |
|---|---|---|---|
| Q1 | Value crates: always on, or one feature each? | (a) jiff/uuid/rust_decimal/serde_json unconditional; (b) `uuid`/`decimal`/`json` features cfg-gating variants | **(a)**. Variant sets that change with features make E3.1b conformance and the sea-query bridge feature-matrix code, and features unify across the graph anyway. jiff is already in core; the others are small, no-default-feature crates. |
| Q2 | NULL typing | (a) untyped `Null` bound as OID 0; (b) `Null(SqlType)` everywhere | **(a)**, gated by `untyped_null_binds_into_int_text_uuid_columns`; if that test fails, add `NullOf(SqlType)` (non-exhaustive, additive) and let `Option<T>` emit it. |
| Q3 | Variants beyond the spec list (`DateTime`, `Time`) | add now / defer | **Add now.** pg `TIMESTAMP` and MySQL `DATETIME` are wall-clock types; forcing them into `Timestamp` would invent a timezone. Same conversion code, ~40 lines. |
| Q4 | pg columns of types outside `SqlType` (inet, interval, arrays, ranges) | (a) error naming column + "cast to text"; (b) fall back to text/bytes like 0.8's silent `""` | **(a)**, with enums mapped to `Text`. Silent garbage is worse than one `::text` cast. |
| Q5 | Text fallbacks in `FromSqlValue` (uuid/temporal/decimal/json accept parseable `Text`) | allow / strict | **Allow** (needed for SQLite and computed MySQL columns); numbers and bools stay strict. |
| Q6 | Trait names | `ToSqlValue`/`FromSqlValue`, or `ToSql`/`FromSql` | **`ToSqlValue`/`FromSqlValue`.** |
| Q7 | Row accessor shape | `get -> Result` only; or sqlx-style panicking `get` + `try_get` | **`get -> Result`, no `try_get`.** If the owner wants the literal `try_get` name, add it as an alias. |

## Non-goals (this leaf)

Arrays, `U64`, interval/network/range types, streaming (`r09-e3-1a-tx-stream`), the `FromRow` derive
(`r09-e3-1a-from-row`), SQLSTATE codes in `SqlError::QueryFailed` (the showcase still matches message
text, `unique_violation.rs:7-9`; a `Constraint` variant can come later), SQLite/MySQL modules (E3.1b).

## Gates for `r09-e3-1a-values-rows`

```bash
ID_EFFECT_REQUIRE_PG=1 cargo nextest run -p id_effect_sql --all-features
cargo clippy -p id_effect_sql --all-targets --all-features -- -D warnings
cargo +1.94 check -p id_effect_sql --all-features         # D10 MSRV
cargo semver-checks -p id_effect_sql                      # output ⊆ the "breaks" rows of section 3
cargo nextest run -p http_showcase && cargo test --doc -p book_snippets   # ripple compiles
```
