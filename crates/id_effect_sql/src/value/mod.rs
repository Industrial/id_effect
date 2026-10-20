//! Typed SQL values: [`SqlValue`] (one bound parameter or column value), its [`SqlType`], and
//! the encoding traits [`ToSqlValue`] / [`FromSqlValue`].
//!
//! Every driver binds and decodes the same variants (`SqlValue` does not change with cargo
//! features). Dates and times are [`jiff`] values: [`SqlValue::Timestamp`] is an absolute
//! instant (pg `TIMESTAMPTZ`), [`SqlValue::DateTime`] a wall-clock date and time (pg `TIMESTAMP`);
//! one is never converted into the other implicitly. Databases store microseconds, so
//! sub-microsecond nanoseconds are truncated.
//!
//! Decoding ([`FromSqlValue`]) never parses text into numbers or booleans, but does parse text
//! into uuids, dates and times, decimals and JSON (SQLite stores those as text). Integers decode
//! from any integer variant, range-checked.
//!
//! ```rust
//! use id_effect_sql::{FromSqlValue, SqlType, SqlValue, ToSqlValue, ValueError, params};
//!
//! let params = params![7i64, "ada", Some(true), None::<i32>];
//! assert_eq!(params[1], SqlValue::Text("ada".into()));
//! assert_eq!(params[3], SqlValue::Null);
//! assert_eq!(i32::from_sql_value(&params[0]), Ok(7));
//! assert_eq!(
//!   bool::from_sql_value(&params[1]),
//!   Err(ValueError::Mismatch { expected: SqlType::Bool, found: SqlType::Text })
//! );
//! assert_eq!(Option::<bool>::from_sql_value(&params[3]), Ok(None));
//! ```

use std::collections::BTreeMap;
use std::fmt;

use id_effect::schema::Unknown;

use crate::error::SqlError;

mod encode;

pub use encode::Json;

/// One typed SQL value: a bound parameter or a decoded column value.
///
/// Non-exhaustive (arrays, unsigned 64-bit and intervals may be added). `PartialEq` only: the
/// float variants have no total equality.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum SqlValue {
  /// SQL NULL, bound with an unspecified type: the database infers it from the statement
  /// (`WHERE uuid_col = $1`, `INSERT … VALUES ($1)`).
  Null,
  /// Boolean.
  Bool(bool),
  /// 16-bit integer (pg `INT2`).
  I16(i16),
  /// 32-bit integer (pg `INT4`).
  I32(i32),
  /// 64-bit integer (pg `INT8`).
  I64(i64),
  /// 32-bit float (pg `FLOAT4`).
  F32(f32),
  /// 64-bit float (pg `FLOAT8`).
  F64(f64),
  /// UTF-8 text (also pg enum values).
  Text(String),
  /// Raw bytes (pg `BYTEA`).
  Bytes(Vec<u8>),
  /// UUID.
  Uuid(uuid::Uuid),
  /// An absolute instant (pg `TIMESTAMPTZ`, MySQL `TIMESTAMP`).
  Timestamp(jiff::Timestamp),
  /// A wall-clock date and time without a time zone (pg `TIMESTAMP`, MySQL `DATETIME`).
  DateTime(jiff::civil::DateTime),
  /// A calendar date.
  Date(jiff::civil::Date),
  /// A time of day.
  Time(jiff::civil::Time),
  /// Exact decimal (pg `NUMERIC` up to 28 significant digits).
  Decimal(rust_decimal::Decimal),
  /// JSON (pg `JSON` and `JSONB`).
  Json(serde_json::Value),
}

/// The type of a [`SqlValue`] (one per variant), and the portable type of a column.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SqlType {
  /// [`SqlValue::Null`] (a NULL of unknown type).
  Null,
  /// [`SqlValue::Bool`].
  Bool,
  /// [`SqlValue::I16`].
  I16,
  /// [`SqlValue::I32`].
  I32,
  /// [`SqlValue::I64`].
  I64,
  /// [`SqlValue::F32`].
  F32,
  /// [`SqlValue::F64`].
  F64,
  /// [`SqlValue::Text`].
  Text,
  /// [`SqlValue::Bytes`].
  Bytes,
  /// [`SqlValue::Uuid`].
  Uuid,
  /// [`SqlValue::Timestamp`].
  Timestamp,
  /// [`SqlValue::DateTime`].
  DateTime,
  /// [`SqlValue::Date`].
  Date,
  /// [`SqlValue::Time`].
  Time,
  /// [`SqlValue::Decimal`].
  Decimal,
  /// [`SqlValue::Json`].
  Json,
}

impl SqlValue {
  /// The value's [`SqlType`].
  pub fn sql_type(&self) -> SqlType {
    match self {
      SqlValue::Null => SqlType::Null,
      SqlValue::Bool(_) => SqlType::Bool,
      SqlValue::I16(_) => SqlType::I16,
      SqlValue::I32(_) => SqlType::I32,
      SqlValue::I64(_) => SqlType::I64,
      SqlValue::F32(_) => SqlType::F32,
      SqlValue::F64(_) => SqlType::F64,
      SqlValue::Text(_) => SqlType::Text,
      SqlValue::Bytes(_) => SqlType::Bytes,
      SqlValue::Uuid(_) => SqlType::Uuid,
      SqlValue::Timestamp(_) => SqlType::Timestamp,
      SqlValue::DateTime(_) => SqlType::DateTime,
      SqlValue::Date(_) => SqlType::Date,
      SqlValue::Time(_) => SqlType::Time,
      SqlValue::Decimal(_) => SqlType::Decimal,
      SqlValue::Json(_) => SqlType::Json,
    }
  }

  /// Whether this is [`SqlValue::Null`].
  #[inline]
  pub fn is_null(&self) -> bool {
    matches!(self, SqlValue::Null)
  }

  /// `value` serialized as [`SqlValue::Json`]; fails with [`SqlError::Decode`] when serde cannot
  /// represent it as JSON (e.g. a map with non-string keys).
  pub fn json<T: serde::Serialize + ?Sized>(value: &T) -> Result<Self, SqlError> {
    serde_json::to_value(value)
      .map(SqlValue::Json)
      .map_err(|e| SqlError::Decode(format!("cannot encode the value as JSON: {e}")))
  }

  /// The value as a schema [`Unknown`] (for decoding rows with a `Schema`): `Null`, `Bool`,
  /// integers as `I64`, floats as `F64`, text, uuids, dates and times (RFC 3339 / ISO 8601) and
  /// decimals as `String`, bytes as an `Array` of `I64`, JSON as the matching tree.
  pub fn to_unknown(&self) -> Unknown {
    match self {
      SqlValue::Null => Unknown::Null,
      SqlValue::Bool(b) => Unknown::Bool(*b),
      SqlValue::I16(n) => Unknown::I64(i64::from(*n)),
      SqlValue::I32(n) => Unknown::I64(i64::from(*n)),
      SqlValue::I64(n) => Unknown::I64(*n),
      SqlValue::F32(x) => Unknown::F64(f64::from(*x)),
      SqlValue::F64(x) => Unknown::F64(*x),
      SqlValue::Bytes(b) => Unknown::Array(b.iter().map(|&x| Unknown::I64(i64::from(x))).collect()),
      SqlValue::Json(j) => json_to_unknown(j),
      other => Unknown::String(other.render()),
    }
  }

  /// The textual rendering behind [`to_unknown`](Self::to_unknown): NULL is empty, bytes are
  /// `{:?}`, JSON is compact, everything else is its `Display`.
  pub(crate) fn render(&self) -> String {
    match self {
      SqlValue::Null => String::new(),
      SqlValue::Bool(b) => b.to_string(),
      SqlValue::I16(n) => n.to_string(),
      SqlValue::I32(n) => n.to_string(),
      SqlValue::I64(n) => n.to_string(),
      SqlValue::F32(x) => x.to_string(),
      SqlValue::F64(x) => x.to_string(),
      SqlValue::Text(s) => s.clone(),
      SqlValue::Bytes(b) => format!("{b:?}"),
      SqlValue::Uuid(u) => u.to_string(),
      SqlValue::Timestamp(t) => t.to_string(),
      SqlValue::DateTime(t) => t.to_string(),
      SqlValue::Date(d) => d.to_string(),
      SqlValue::Time(t) => t.to_string(),
      SqlValue::Decimal(d) => d.to_string(),
      SqlValue::Json(j) => j.to_string(),
    }
  }
}

fn json_to_unknown(value: &serde_json::Value) -> Unknown {
  match value {
    serde_json::Value::Null => Unknown::Null,
    serde_json::Value::Bool(b) => Unknown::Bool(*b),
    serde_json::Value::Number(n) => match n.as_i64() {
      Some(i) => Unknown::I64(i),
      None => Unknown::F64(n.as_f64().unwrap_or(f64::NAN)),
    },
    serde_json::Value::String(s) => Unknown::String(s.clone()),
    serde_json::Value::Array(items) => Unknown::Array(items.iter().map(json_to_unknown).collect()),
    serde_json::Value::Object(map) => Unknown::Object(
      map
        .iter()
        .map(|(k, v)| (k.clone(), json_to_unknown(v)))
        .collect::<BTreeMap<_, _>>(),
    ),
  }
}

/// Encode a Rust value as a [`SqlValue`] (by reference, so entities encode fields without
/// moving them). Implement it for a domain type by delegating to the wrapped value.
///
/// ```rust
/// use id_effect_sql::{SqlValue, ToSqlValue};
///
/// struct Email(String);
/// impl ToSqlValue for Email {
///   fn to_sql_value(&self) -> SqlValue {
///     self.0.to_sql_value()
///   }
/// }
/// assert_eq!(Email("a@b.c".into()).to_sql_value(), SqlValue::Text("a@b.c".into()));
/// ```
pub trait ToSqlValue {
  /// The value as a [`SqlValue`].
  fn to_sql_value(&self) -> SqlValue;
}

/// Decode a Rust value from a [`SqlValue`] (see the [module docs](self) for what each type
/// accepts). [`SqlRow::get`](crate::SqlRow::get) adds the column to the error.
///
/// ```rust
/// use id_effect_sql::{FromSqlValue, SqlValue, ValueError};
///
/// struct Email(String);
/// impl FromSqlValue for Email {
///   fn from_sql_value(v: &SqlValue) -> Result<Self, ValueError> {
///     let s = String::from_sql_value(v)?;
///     if s.contains('@') { Ok(Email(s)) } else { Err(ValueError::Invalid(format!("not an email: {s}"))) }
///   }
/// }
/// assert!(Email::from_sql_value(&SqlValue::Text("a@b.c".into())).is_ok());
/// assert!(matches!(Email::from_sql_value(&SqlValue::Text("x".into())), Err(ValueError::Invalid(_))));
/// ```
pub trait FromSqlValue: Sized {
  /// Decode `v`.
  fn from_sql_value(v: &SqlValue) -> Result<Self, ValueError>;
}

/// Why a [`SqlValue`] did not decode into a Rust type (column-free; [`SqlError`] adds the
/// column, see [`SqlError::from_value_error`]).
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ValueError {
  /// The value is NULL but the type is not an `Option`.
  UnexpectedNull,
  /// The value's type is not one the Rust type decodes from.
  Mismatch {
    /// The type the Rust type decodes from.
    expected: SqlType,
    /// The value's type.
    found: SqlType,
  },
  /// The value does not fit (an `i64` into an `i32`, a decimal beyond 28 digits, …).
  OutOfRange(String),
  /// The value is malformed (text that does not parse, JSON of the wrong shape, …).
  Invalid(String),
}

impl fmt::Display for ValueError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      ValueError::UnexpectedNull => write!(f, "unexpected NULL"),
      ValueError::Mismatch { expected, found } => {
        write!(f, "expected {expected:?}, found {found:?}")
      }
      ValueError::OutOfRange(m) => write!(f, "out of range: {m}"),
      ValueError::Invalid(m) => write!(f, "invalid: {m}"),
    }
  }
}

impl std::error::Error for ValueError {}

/// A `Vec<SqlValue>` of the arguments, each encoded with [`ToSqlValue`] (by reference).
///
/// ```rust
/// use id_effect_sql::{SqlValue, params};
///
/// let (id, title) = (7i64, String::from("write docs"));
/// assert_eq!(params![id, title, true], vec![
///   SqlValue::I64(7),
///   SqlValue::Text("write docs".into()),
///   SqlValue::Bool(true),
/// ]);
/// assert!(params![].is_empty());
/// ```
#[macro_export]
macro_rules! params {
  () => {
    ::std::vec::Vec::<$crate::SqlValue>::new()
  };
  ($($v:expr),+ $(,)?) => {
    ::std::vec![$($crate::ToSqlValue::to_sql_value(&$v)),+]
  };
}

#[cfg(test)]
mod tests {
  use std::collections::BTreeMap;

  use id_effect::schema::Unknown;
  use serde_json::json;

  use super::*;

  fn samples() -> Vec<SqlValue> {
    vec![
      SqlValue::Null,
      SqlValue::Bool(true),
      SqlValue::I16(-2),
      SqlValue::I32(3),
      SqlValue::I64(4),
      SqlValue::F32(1.5),
      SqlValue::F64(2.25),
      SqlValue::Text("t".into()),
      SqlValue::Bytes(vec![1, 2]),
      SqlValue::Uuid(uuid::Uuid::nil()),
      SqlValue::Timestamp(jiff::Timestamp::UNIX_EPOCH),
      SqlValue::DateTime(jiff::civil::date(2024, 2, 29).at(13, 4, 5, 0)),
      SqlValue::Date(jiff::civil::date(2024, 2, 29)),
      SqlValue::Time(jiff::civil::time(13, 4, 5, 6_000)),
      SqlValue::Decimal(rust_decimal::Decimal::new(12345, 2)),
      SqlValue::Json(json!({"a": [1, 2.5, "x", null, true]})),
    ]
  }

  #[test]
  fn every_variant_has_its_own_type() {
    let types: Vec<SqlType> = samples().iter().map(SqlValue::sql_type).collect();
    let distinct: std::collections::HashSet<_> = types.iter().copied().collect();
    assert_eq!(distinct.len(), types.len());
    assert!(SqlValue::Null.is_null());
    assert!(!SqlValue::I64(0).is_null());
  }

  #[test]
  fn render_matches_the_0_8_strings_for_the_legacy_kinds() {
    let rendered: Vec<String> = samples().iter().map(SqlValue::render).collect();
    assert_eq!(
      rendered,
      [
        "",
        "true",
        "-2",
        "3",
        "4",
        "1.5",
        "2.25",
        "t",
        "[1, 2]",
        "00000000-0000-0000-0000-000000000000",
        "1970-01-01T00:00:00Z",
        "2024-02-29T13:04:05",
        "2024-02-29",
        "13:04:05.000006",
        "123.45",
        r#"{"a":[1,2.5,"x",null,true]}"#,
      ]
    );
  }

  #[test]
  fn to_unknown_maps_every_variant() {
    let unknowns: Vec<Unknown> = samples().iter().map(SqlValue::to_unknown).collect();
    assert_eq!(unknowns[0], Unknown::Null);
    assert_eq!(unknowns[1], Unknown::Bool(true));
    assert_eq!(
      unknowns[2..5],
      [Unknown::I64(-2), Unknown::I64(3), Unknown::I64(4)]
    );
    assert_eq!(unknowns[5..7], [Unknown::F64(1.5), Unknown::F64(2.25)]);
    assert_eq!(unknowns[7], Unknown::String("t".into()));
    assert_eq!(
      unknowns[8],
      Unknown::Array(vec![Unknown::I64(1), Unknown::I64(2)])
    );
    assert_eq!(unknowns[10], Unknown::String("1970-01-01T00:00:00Z".into()));
    assert_eq!(unknowns[14], Unknown::String("123.45".into()));
    assert_eq!(
      unknowns[15],
      Unknown::Object(BTreeMap::from([(
        "a".to_owned(),
        Unknown::Array(vec![
          Unknown::I64(1),
          Unknown::F64(2.5),
          Unknown::String("x".into()),
          Unknown::Null,
          Unknown::Bool(true),
        ])
      )]))
    );
    assert_eq!(
      SqlValue::Json(json!(u64::MAX)).to_unknown(),
      Unknown::F64(u64::MAX as f64)
    );
  }

  #[test]
  fn json_constructor_serializes_or_fails() {
    #[derive(serde::Serialize)]
    struct Tag {
      name: &'static str,
    }
    assert_eq!(
      SqlValue::json(&Tag { name: "x" }).unwrap(),
      SqlValue::Json(json!({"name": "x"}))
    );
    let bad = BTreeMap::from([((1, 2), 3)]);
    assert!(matches!(SqlValue::json(&bad), Err(SqlError::Decode(_))));
  }

  #[test]
  fn value_errors_display() {
    assert_eq!(ValueError::UnexpectedNull.to_string(), "unexpected NULL");
    assert_eq!(
      ValueError::Mismatch {
        expected: SqlType::Uuid,
        found: SqlType::I64
      }
      .to_string(),
      "expected Uuid, found I64"
    );
    assert_eq!(
      ValueError::OutOfRange("x".into()).to_string(),
      "out of range: x"
    );
    assert_eq!(ValueError::Invalid("y".into()).to_string(), "invalid: y");
  }
}
