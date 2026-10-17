//! [`SqlValue`] ↔ MySQL: binding parameters and decoding result rows (see the
//! [module docs](super) for the mapping).
//!
//! Decoding switches on each column's type (as sqlx names it) once per result set and shares one
//! `Arc<[SqlColumn]>` among its rows. Columns of a type outside [`SqlType`] (`BIT`, `GEOMETRY`)
//! are an error naming the column and suggesting a cast. A value that does not fit its portable
//! type (`BIGINT UNSIGNED` beyond `i64::MAX`, a `TIME` outside a day, a zero date) is
//! [`SqlError::ValueOutOfRange`], never a panic.

use std::sync::Arc;

use rust_decimal::Decimal;
use sqlx::mysql::{MySql, MySqlArguments, MySqlRow};
use sqlx::{Column as _, Row as _, TypeInfo as _, ValueRef as _};

use crate::value::time_conv;
use crate::{SqlColumn, SqlError, SqlRow, SqlType, SqlValue};

pub(super) type MySqlQuery<'q> = sqlx::query::Query<'q, MySql, MySqlArguments>;

/// `query` with `params` bound to the `?` placeholders in order.
pub(super) fn bind_query<'q>(
  mut query: MySqlQuery<'q>,
  params: &[SqlValue],
) -> Result<MySqlQuery<'q>, SqlError> {
  for (i, param) in params.iter().enumerate() {
    let out_of_range = |message: String| SqlError::ValueOutOfRange {
      column: format!("${}", i + 1),
      message,
    };
    query = match param {
      SqlValue::Null => query.bind(None::<i64>),
      SqlValue::Bool(v) => query.bind(*v),
      SqlValue::I16(v) => query.bind(*v),
      SqlValue::I32(v) => query.bind(*v),
      SqlValue::I64(v) => query.bind(*v),
      SqlValue::F32(v) => query.bind(*v),
      SqlValue::F64(v) => query.bind(*v),
      SqlValue::Text(v) => query.bind(v.clone()),
      SqlValue::Bytes(v) => query.bind(v.clone()),
      SqlValue::Uuid(v) => query.bind(v.as_bytes().to_vec()),
      SqlValue::Timestamp(v) => {
        let utc = jiff::tz::Offset::UTC.to_datetime(*v);
        year_in_range(utc.date()).map_err(out_of_range)?;
        query.bind(time_conv::timestamp(*v).map_err(out_of_range)?)
      }
      SqlValue::DateTime(v) => {
        year_in_range(v.date()).map_err(out_of_range)?;
        query.bind(time_conv::datetime(*v).map_err(out_of_range)?)
      }
      SqlValue::Date(v) => {
        year_in_range(*v).map_err(out_of_range)?;
        query.bind(time_conv::date(*v).map_err(out_of_range)?)
      }
      SqlValue::Time(v) => query.bind(time_conv::time_of_day(*v).map_err(out_of_range)?),
      SqlValue::Decimal(v) => query.bind(*v),
      SqlValue::Json(v) => query.bind(sqlx::types::Json(v.clone())),
    };
  }
  Ok(query)
}

/// MySQL dates are in years 0000-9999 (jiff's reach back to -9999).
fn year_in_range(d: jiff::civil::Date) -> Result<(), String> {
  if d.year() < 0 {
    return Err(format!("{d}: MySQL dates are in years 0000-9999"));
  }
  Ok(())
}

/// How a column's non-NULL values decode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
  /// `TINYINT(1)` / `BOOLEAN`: 0 or 1.
  Bool,
  /// A signed integer, narrowed to the column's portable type.
  Signed,
  /// An unsigned integer (or `YEAR`), narrowed to the column's portable type.
  Unsigned,
  F32,
  F64,
  Text,
  Bytes,
  Json,
  Timestamp,
  DateTime,
  Date,
  Time,
  Decimal,
  /// The type of a bare `NULL` (every value is NULL).
  Null,
}

/// The decoding and portable type of a MySQL column type (as sqlx names it), or `None` when it
/// has none.
fn kind_of(name: &str) -> Option<(Kind, SqlType)> {
  Some(match name {
    "BOOLEAN" => (Kind::Bool, SqlType::Bool),
    "TINYINT" | "SMALLINT" => (Kind::Signed, SqlType::I16),
    "MEDIUMINT" | "INT" => (Kind::Signed, SqlType::I32),
    "BIGINT" => (Kind::Signed, SqlType::I64),
    // Each unsigned type reads as the narrowest signed type holding all its values.
    "TINYINT UNSIGNED" | "YEAR" => (Kind::Unsigned, SqlType::I16),
    "SMALLINT UNSIGNED" | "MEDIUMINT UNSIGNED" => (Kind::Unsigned, SqlType::I32),
    "INT UNSIGNED" | "BIGINT UNSIGNED" => (Kind::Unsigned, SqlType::I64),
    "FLOAT" => (Kind::F32, SqlType::F32),
    "DOUBLE" => (Kind::F64, SqlType::F64),
    "CHAR" | "VARCHAR" | "TINYTEXT" | "TEXT" | "MEDIUMTEXT" | "LONGTEXT" | "ENUM" | "SET" => {
      (Kind::Text, SqlType::Text)
    }
    "BINARY" | "VARBINARY" | "TINYBLOB" | "BLOB" | "MEDIUMBLOB" | "LONGBLOB" => {
      (Kind::Bytes, SqlType::Bytes)
    }
    "JSON" => (Kind::Json, SqlType::Json),
    "TIMESTAMP" => (Kind::Timestamp, SqlType::Timestamp),
    "DATETIME" => (Kind::DateTime, SqlType::DateTime),
    "DATE" => (Kind::Date, SqlType::Date),
    "TIME" => (Kind::Time, SqlType::Time),
    "DECIMAL" => (Kind::Decimal, SqlType::Decimal),
    "NULL" => (Kind::Null, SqlType::Null),
    _ => return None,
  })
}

/// Decodes a result set: one shared column list, then every row.
pub(super) fn decode_rows(rows: &[MySqlRow]) -> Result<Vec<SqlRow>, SqlError> {
  let Some(first) = rows.first() else {
    return Ok(Vec::new());
  };
  let mut kinds = Vec::with_capacity(first.len());
  let mut columns = Vec::with_capacity(first.len());
  for column in first.columns() {
    let (name, ty) = (column.name(), column.type_info().name());
    let Some((kind, sql_type)) = kind_of(ty) else {
      return Err(SqlError::Unsupported(format!(
        "column `{name}` has type {ty}; cast it (`CAST({name} AS CHAR)`)"
      )));
    };
    kinds.push((kind, sql_type));
    columns.push(SqlColumn::new(name, sql_type).with_db_type(ty));
  }
  let columns: Arc<[SqlColumn]> = columns.into();
  rows
    .iter()
    .map(|row| {
      let values = kinds
        .iter()
        .enumerate()
        .map(|(i, (kind, sql_type))| {
          decode(row, i, *kind, *sql_type).map_err(|message| SqlError::ValueOutOfRange {
            column: columns[i].name().to_owned(),
            message,
          })
        })
        .collect::<Result<Vec<_>, _>>()?;
      SqlRow::from_parts(Arc::clone(&columns), values)
    })
    .collect()
}

fn decode(row: &MySqlRow, i: usize, kind: Kind, sql_type: SqlType) -> Result<SqlValue, String> {
  let err = |e: sqlx::Error| e.to_string();
  if row.try_get_raw(i).map_err(err)?.is_null() {
    return Ok(SqlValue::Null);
  }
  Ok(match kind {
    Kind::Bool => match row.try_get_unchecked::<i64, _>(i).map_err(err)? {
      0 => SqlValue::Bool(false),
      1 => SqlValue::Bool(true),
      n => return Err(format!("BOOLEAN column holds {n}")),
    },
    Kind::Signed => integer(row.try_get_unchecked::<i64, _>(i).map_err(err)?, sql_type)?,
    Kind::Unsigned => {
      let n: u64 = row.try_get_unchecked(i).map_err(err)?;
      let n = i64::try_from(n).map_err(|_| format!("unsigned value {n} is beyond i64::MAX"))?;
      integer(n, sql_type)?
    }
    Kind::F32 => SqlValue::F32(row.try_get_unchecked(i).map_err(err)?),
    Kind::F64 => SqlValue::F64(row.try_get_unchecked(i).map_err(err)?),
    Kind::Text => SqlValue::Text(row.try_get_unchecked(i).map_err(err)?),
    Kind::Bytes => SqlValue::Bytes(row.try_get_unchecked(i).map_err(err)?),
    Kind::Json => {
      let text: &str = row.try_get_unchecked(i).map_err(err)?;
      SqlValue::Json(serde_json::from_str(text).map_err(|e| e.to_string())?)
    }
    Kind::Timestamp => {
      let t: time::OffsetDateTime = row.try_get_unchecked(i).map_err(err)?;
      SqlValue::Timestamp(timestamp(t)?)
    }
    Kind::DateTime => SqlValue::DateTime(datetime(row.try_get_unchecked(i).map_err(err)?)?),
    Kind::Date => SqlValue::Date(date(row.try_get_unchecked(i).map_err(err)?)?),
    Kind::Time => SqlValue::Time(time_of_day(row.try_get_unchecked(i).map_err(err)?)?),
    Kind::Decimal => SqlValue::Decimal(decimal(row.try_get_unchecked(i).map_err(err)?)?),
    Kind::Null => SqlValue::Null,
  })
}

/// `n` as the integer variant of `sql_type` (the column type bounds it, so this only fails on a
/// server bug).
fn integer(n: i64, sql_type: SqlType) -> Result<SqlValue, String> {
  let out_of_range = |_| format!("{n} does not fit {sql_type:?}");
  Ok(match sql_type {
    SqlType::I16 => SqlValue::I16(i16::try_from(n).map_err(out_of_range)?),
    SqlType::I32 => SqlValue::I32(i32::try_from(n).map_err(out_of_range)?),
    _ => SqlValue::I64(n),
  })
}

/// A `DECIMAL`, exactly. MySQL writes every digit of the column's scale, so trailing fractional
/// zeros are dropped when they would take the value beyond `Decimal`'s 28 digits
/// (`DECIMAL(38,18)` holding `1.5`); a value that still needs more digits is out of range,
/// never rounded.
fn decimal(text: &str) -> Result<Decimal, String> {
  let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
  let fits = |fraction: &str| {
    let digits = format!("{}{fraction}", whole.trim_start_matches('-'));
    digits.trim_start_matches('0').len() <= 28 && fraction.len() <= 28
  };
  let fraction = if fits(fraction) {
    fraction
  } else {
    fraction.trim_end_matches('0')
  };
  if !fits(fraction) {
    return Err(format!("DECIMAL {text} needs more than 28 digits"));
  }
  let exact = match fraction {
    "" => whole.to_owned(),
    fraction => format!("{whole}.{fraction}"),
  };
  exact.parse().map_err(|e| format!("DECIMAL {text}: {e}"))
}

/// A `TIMESTAMP` (read in the session's UTC time zone) as an instant.
fn timestamp(t: time::OffsetDateTime) -> Result<jiff::Timestamp, String> {
  jiff::Timestamp::from_nanosecond(t.unix_timestamp_nanos()).map_err(|e| e.to_string())
}

fn date(d: time::Date) -> Result<jiff::civil::Date, String> {
  let year = i16::try_from(d.year()).map_err(|e| e.to_string())?;
  jiff::civil::Date::new(year, u8::from(d.month()) as i8, d.day() as i8).map_err(|e| e.to_string())
}

fn time_of_day(t: time::Time) -> Result<jiff::civil::Time, String> {
  jiff::civil::Time::new(
    t.hour() as i8,
    t.minute() as i8,
    t.second() as i8,
    t.nanosecond() as i32,
  )
  .map_err(|e| e.to_string())
}

fn datetime(dt: time::PrimitiveDateTime) -> Result<jiff::civil::DateTime, String> {
  Ok(date(dt.date())?.to_datetime(time_of_day(dt.time())?))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn type_names_map_to_kinds() {
    for (name, kind, sql_type) in [
      ("BOOLEAN", Kind::Bool, SqlType::Bool),
      ("TINYINT", Kind::Signed, SqlType::I16),
      ("INT", Kind::Signed, SqlType::I32),
      ("BIGINT", Kind::Signed, SqlType::I64),
      ("YEAR", Kind::Unsigned, SqlType::I16),
      ("MEDIUMINT UNSIGNED", Kind::Unsigned, SqlType::I32),
      ("BIGINT UNSIGNED", Kind::Unsigned, SqlType::I64),
      ("ENUM", Kind::Text, SqlType::Text),
      ("VARBINARY", Kind::Bytes, SqlType::Bytes),
      ("TIMESTAMP", Kind::Timestamp, SqlType::Timestamp),
      ("DATETIME", Kind::DateTime, SqlType::DateTime),
      ("NULL", Kind::Null, SqlType::Null),
    ] {
      assert_eq!(kind_of(name), Some((kind, sql_type)), "{name}");
    }
    assert_eq!(kind_of("BIT"), None);
    assert_eq!(kind_of("GEOMETRY"), None);
  }

  #[test]
  fn integers_narrow_to_their_column_type() {
    assert_eq!(integer(-7, SqlType::I16), Ok(SqlValue::I16(-7)));
    assert_eq!(integer(70_000, SqlType::I32), Ok(SqlValue::I32(70_000)));
    assert_eq!(integer(i64::MIN, SqlType::I64), Ok(SqlValue::I64(i64::MIN)));
    assert!(integer(70_000, SqlType::I16).unwrap_err().contains("I16"));
    assert!(integer(i64::MAX, SqlType::I32).is_err());
  }

  #[test]
  fn decimals_keep_their_scale_unless_it_is_too_wide() {
    assert_eq!(decimal("-1.50").unwrap().to_string(), "-1.50");
    let wide = "9223372036854775807.000000000000000000";
    assert_eq!(decimal(wide), Ok(Decimal::from(i64::MAX)));
    assert_eq!(
      decimal("0.100000000000000000000000000000"),
      Ok(Decimal::new(1, 1))
    );
    assert!(
      decimal("123456789012345678901234567890123")
        .unwrap_err()
        .contains("28 digits")
    );
    assert!(
      decimal("0.12345678901234567890123456789").is_err(),
      "never rounded"
    );
    assert!(decimal("1.x").unwrap_err().starts_with("DECIMAL 1.x:"));
  }

  #[test]
  fn temporals_convert_from_time_and_check_the_year() {
    let t = time::OffsetDateTime::from_unix_timestamp_nanos(1_000_001_000).unwrap();
    assert_eq!(
      timestamp(t),
      Ok(jiff::Timestamp::from_nanosecond(1_000_001_000).unwrap())
    );
    let dt = time::PrimitiveDateTime::new(
      time::Date::from_calendar_date(1, time::Month::February, 3).unwrap(),
      time::Time::from_hms_micro(4, 5, 6, 7).unwrap(),
    );
    assert_eq!(
      datetime(dt),
      Ok(jiff::civil::date(1, 2, 3).at(4, 5, 6, 7_000))
    );
    assert!(year_in_range(jiff::civil::date(0, 1, 1)).is_ok());
    assert!(
      year_in_range(jiff::civil::date(-1, 1, 1))
        .unwrap_err()
        .contains("0000-9999")
    );
  }
}
