//! The `sea_query::Value` ↔ [`SqlValue`] bridge (D3: our own, no `sea-query-binder`); the table
//! is on [`sql_values`].

use sea_query::{OptionEnum, Value, Values};

use super::time_conv as tc;
use crate::error::SqlError;
use crate::value::SqlValue;

/// Why one `sea_query::Value` has no [`SqlValue`].
enum Unbridged {
  /// The value does not fit (an unsigned above `i64::MAX`, a date jiff cannot hold).
  OutOfRange(String),
  /// The variant has no [`SqlValue`] (arrays, chrono, …).
  Unsupported(String),
}

impl Unbridged {
  fn into_error(self, column: String) -> SqlError {
    match self {
      Unbridged::OutOfRange(message) => SqlError::ValueOutOfRange { column, message },
      Unbridged::Unsupported(what) => {
        SqlError::Unsupported(format!("{column}: sea-query value {what} has no SqlValue"))
      }
    }
  }
}

fn some<T>(v: Option<T>, f: impl FnOnce(T) -> SqlValue) -> SqlValue {
  v.map_or(SqlValue::Null, f)
}

fn some_try<T>(
  v: Option<T>,
  f: impl FnOnce(T) -> Result<SqlValue, String>,
) -> Result<SqlValue, Unbridged> {
  v.map_or(Ok(SqlValue::Null), |x| f(x).map_err(Unbridged::OutOfRange))
}

fn bridge(v: Value) -> Result<SqlValue, Unbridged> {
  Ok(match v {
    Value::Bool(b) => some(b, SqlValue::Bool),
    Value::TinyInt(n) => some(n, |n| SqlValue::I16(n.into())),
    Value::SmallInt(n) => some(n, SqlValue::I16),
    Value::Int(n) => some(n, SqlValue::I32),
    Value::BigInt(n) => some(n, SqlValue::I64),
    Value::TinyUnsigned(n) => some(n, |n| SqlValue::I16(n.into())),
    Value::SmallUnsigned(n) => some(n, |n| SqlValue::I32(n.into())),
    Value::Unsigned(n) => some(n, |n| SqlValue::I64(n.into())),
    Value::BigUnsigned(n) => some_try(n, |n| {
      i64::try_from(n)
        .map(SqlValue::I64)
        .map_err(|_| format!("{n} does not fit an I64"))
    })?,
    Value::Float(x) => some(x, SqlValue::F32),
    Value::Double(x) => some(x, SqlValue::F64),
    Value::String(s) => some(s, SqlValue::Text),
    Value::Char(c) => some(c, |c| SqlValue::Text(c.to_string())),
    Value::Enum(OptionEnum::Some(e)) => SqlValue::Text(e.value.into_owned()),
    Value::Enum(OptionEnum::None(_)) => SqlValue::Null,
    Value::Bytes(b) => some(b, SqlValue::Bytes),
    Value::Json(j) => some(j, |j| SqlValue::Json(*j)),
    Value::Uuid(u) => some(u, SqlValue::Uuid),
    Value::Decimal(d) => some(d, SqlValue::Decimal),
    Value::JiffTimestamp(t) => some(t, |t| SqlValue::Timestamp(*t)),
    Value::JiffDateTime(dt) => some(dt, |dt| SqlValue::DateTime(*dt)),
    Value::JiffDate(d) => some(d, SqlValue::Date),
    Value::JiffTime(t) => some(t, SqlValue::Time),
    Value::TimeDateTimeWithTimeZone(t) => {
      some_try(t, |t| tc::timestamp(t).map(SqlValue::Timestamp))?
    }
    Value::TimeDateTime(dt) => some_try(dt, |dt| tc::datetime(dt).map(SqlValue::DateTime))?,
    Value::TimeDate(d) => some_try(d, |d| tc::date(d).map(SqlValue::Date))?,
    Value::TimeTime(t) => some(t, |t| SqlValue::Time(tc::time_of_day(t))),
    // Variants other crates' sea-query features add (arrays, chrono, bigdecimal, …).
    #[allow(unreachable_patterns)]
    other => return Err(Unbridged::Unsupported(format!("{other:?}"))),
  })
}

impl TryFrom<Value> for SqlValue {
  type Error = SqlError;

  /// The [`SqlValue`] of one sea-query value (the table is on [`sql_values`]); an error names
  /// the value as `value`.
  fn try_from(v: Value) -> Result<Self, SqlError> {
    bridge(v).map_err(|e| e.into_error("value".into()))
  }
}

/// The [`SqlValue`]s of a built statement's parameters, in placeholder order; an error names the
/// parameter (`$2`). One value converts with `SqlValue::try_from(value)` (an error names it
/// `value`).
///
/// | `sea_query::Value` | [`SqlValue`] |
/// |---|---|
/// | any `X(None)` (a typed NULL) | [`Null`](SqlValue::Null) |
/// | `Bool`, `TinyInt`/`SmallInt`, `Int`, `BigInt` | `Bool`, `I16`, `I32`, `I64` |
/// | `TinyUnsigned`, `SmallUnsigned`, `Unsigned`, `BigUnsigned` | `I16`, `I32`, `I64`, `I64` (above `i64::MAX`: [`SqlError::ValueOutOfRange`]) |
/// | `Float`, `Double`, `String`, `Char`, `Enum`, `Bytes` | `F32`, `F64`, `Text`, `Text`, `Text` (the label), `Bytes` |
/// | `Json`, `Uuid`, `Decimal` | `Json`, `Uuid`, `Decimal` |
/// | `JiffTimestamp`, `JiffDateTime`, `JiffDate`, `JiffTime` | `Timestamp`, `DateTime`, `Date`, `Time` |
/// | `TimeDateTimeWithTimeZone`, `TimeDateTime`, `TimeDate`, `TimeTime` | `Timestamp` (the instant; the offset is dropped), `DateTime`, `Date`, `Time` |
/// | anything else (arrays, chrono, … when another crate enables them) | [`SqlError::Unsupported`] |
///
/// The other way, `From<SqlValue> for sea_query::Value` maps every variant to its own (dates and
/// times to the `Jiff*` variants, `Null` to `String(None)`), so `SqlValue → Value → SqlValue` is
/// the identity, and any [`ToSqlValue`](crate::ToSqlValue) type goes into a builder as
/// `Expr::val(x.to_sql_value())`.
pub fn sql_values(values: Values) -> Result<Vec<SqlValue>, SqlError> {
  values
    .0
    .into_iter()
    .enumerate()
    .map(|(i, v)| bridge(v).map_err(|e| e.into_error(format!("${}", i + 1))))
    .collect()
}

impl From<SqlValue> for Value {
  /// Every variant to its own (`Null` becomes `String(None)`, rendered as an untyped NULL
  /// parameter; dates and times become the `Jiff*` variants).
  fn from(v: SqlValue) -> Self {
    match v {
      SqlValue::Null => Value::String(None),
      SqlValue::Bool(b) => b.into(),
      SqlValue::I16(n) => n.into(),
      SqlValue::I32(n) => n.into(),
      SqlValue::I64(n) => n.into(),
      SqlValue::F32(x) => x.into(),
      SqlValue::F64(x) => x.into(),
      SqlValue::Text(s) => s.into(),
      SqlValue::Bytes(b) => b.into(),
      SqlValue::Uuid(u) => u.into(),
      SqlValue::Timestamp(t) => t.into(),
      SqlValue::DateTime(dt) => dt.into(),
      SqlValue::Date(d) => d.into(),
      SqlValue::Time(t) => t.into(),
      SqlValue::Decimal(d) => d.into(),
      SqlValue::Json(j) => j.into(),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn an_unsupported_variant_names_the_parameter() {
    // No variant is unsupported under this crate's sea-query features; the arm is for
    // features another crate turns on.
    assert_eq!(
      Unbridged::Unsupported("Array(..)".into()).into_error("$3".into()),
      SqlError::Unsupported("$3: sea-query value Array(..) has no SqlValue".into())
    );
  }
}
