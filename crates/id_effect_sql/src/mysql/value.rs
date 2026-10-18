//! [`SqlValue`] ↔ MySQL: binding parameters and decoding result rows (see the
//! [module docs](super) for the mapping).
//!
//! Decoding switches on each column's type (as sqlx names it) once per result set and shares one
//! `Arc<[SqlColumn]>` among its rows. Columns of a type outside [`SqlType`] (`BIT`, `GEOMETRY`)
//! are an error naming the column and suggesting a cast. A value that does not fit its portable
//! type (`BIGINT UNSIGNED` beyond `i64::MAX`, a `TIME` outside a day, a zero date) is
//! [`SqlError::ValueOutOfRange`], never a panic.
//!
//! Date-times are our own conversions (sqlx's `time` and `chrono` codecs are not used): `DATE`,
//! `DATETIME` and `TIMESTAMP` decode from the binary protocol's bytes (text-protocol values are
//! parsed too), `TIME` through sqlx's `MySqlTime`. sqlx has no public type id for `DATE` /
//! `DATETIME` / `TIMESTAMP` parameters, so those bind as MySQL literals
//! (`'2024-02-29 12:34:56.123456'`), which the server converts on assignment and comparison; a
//! `TIME` binds as `TIME`.

use std::sync::Arc;

use jiff::civil::{Date, DateTime, Time};
use rust_decimal::Decimal;
use sqlx::mysql::types::{MySqlTime, MySqlTimeSign};
use sqlx::mysql::{MySql, MySqlArguments, MySqlRow};
use sqlx::{Column as _, Row as _, TypeInfo as _, ValueRef as _};

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
      // The session time zone is UTC, so a `TIMESTAMP` literal is the instant's UTC wall clock.
      SqlValue::Timestamp(v) => {
        let utc = jiff::tz::Offset::UTC.to_datetime(*v);
        query.bind(datetime_literal(utc).map_err(out_of_range)?)
      }
      SqlValue::DateTime(v) => query.bind(datetime_literal(*v).map_err(out_of_range)?),
      SqlValue::Date(v) => query.bind(date_literal(*v).map_err(out_of_range)?),
      SqlValue::Time(v) => query.bind(time_to_wire(*v).map_err(out_of_range)?),
      SqlValue::Decimal(v) => query.bind(*v),
      SqlValue::Json(v) => query.bind(sqlx::types::Json(v.clone())),
    };
  }
  Ok(query)
}

/// MySQL dates are in years 0000-9999 (jiff's reach back to -9999).
fn year_in_range(d: Date) -> Result<(), String> {
  if d.year() < 0 {
    return Err(format!("{d}: MySQL dates are in years 0000-9999"));
  }
  Ok(())
}

/// A date as MySQL's literal `YYYY-MM-DD`.
fn date_literal(d: Date) -> Result<String, String> {
  year_in_range(d)?;
  Ok(format!("{:04}-{:02}-{:02}", d.year(), d.month(), d.day()))
}

/// A date and time as MySQL's literal `YYYY-MM-DD hh:mm:ss.ffffff` (sub-microseconds truncated,
/// as the binary protocol would).
fn datetime_literal(dt: DateTime) -> Result<String, String> {
  Ok(format!(
    "{} {:02}:{:02}:{:02}.{:06}",
    date_literal(dt.date())?,
    dt.hour(),
    dt.minute(),
    dt.second(),
    dt.subsec_nanosecond() / 1_000
  ))
}

/// A time of day as a `TIME` (sub-microseconds truncated).
fn time_to_wire(t: Time) -> Result<MySqlTime, String> {
  MySqlTime::new(
    MySqlTimeSign::Positive,
    u32::from(t.hour().unsigned_abs()),
    t.minute().unsigned_abs(),
    t.second().unsigned_abs(),
    t.subsec_nanosecond().unsigned_abs() / 1_000,
  )
  .map_err(|e| e.to_string())
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
    Kind::Timestamp => SqlValue::Timestamp(timestamp(datetime(
      row.try_get_unchecked(i).map_err(err)?,
    )?)?),
    Kind::DateTime => SqlValue::DateTime(datetime(row.try_get_unchecked(i).map_err(err)?)?),
    Kind::Date => SqlValue::Date(datetime(row.try_get_unchecked(i).map_err(err)?)?.date()),
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
fn timestamp(utc: DateTime) -> Result<jiff::Timestamp, String> {
  jiff::tz::Offset::UTC
    .to_timestamp(utc)
    .map_err(|e| e.to_string())
}

/// A `DATE`, `DATETIME` or `TIMESTAMP` value's bytes as a date and time (midnight for a `DATE`).
///
/// Binary protocol: a length byte, then the year (`u16`, little-endian), month and day; hour,
/// minute and second unless the time is midnight (length 4); the microseconds (`u32`,
/// little-endian) unless they are 0 (length 7). A zero date has length 0 and reads as NULL
/// (sqlx). Text protocol: `YYYY-MM-DD[ hh:mm:ss[.ffffff]]`. Every field is checked by jiff's
/// fallible constructors (an invalid date such as `2024-02-30` is an error, never a panic).
fn datetime(bytes: &[u8]) -> Result<DateTime, String> {
  if bytes.first().is_some_and(u8::is_ascii_digit) {
    let text = std::str::from_utf8(bytes).map_err(|e| e.to_string())?;
    return text
      .replacen(' ', "T", 1)
      .parse()
      .map_err(|e| format!("date-time {text:?}: {e}"));
  }
  let wire = match bytes {
    [len @ (4 | 7 | 11), rest @ ..] if rest.len() == usize::from(*len) => rest,
    _ => {
      return Err(format!(
        "a date-time of {} bytes is not MySQL's binary format",
        bytes.len()
      ));
    }
  };
  let year = u16::from_le_bytes([wire[0], wire[1]]);
  let year = i16::try_from(year).map_err(|_| format!("date-time year {year} is out of range"))?;
  let date = Date::new(year, small(wire[2])?, small(wire[3])?).map_err(|e| e.to_string())?;
  let (hour, minute, second) = wire.get(4..7).map_or((0, 0, 0), |t| (t[0], t[1], t[2]));
  let micros = wire
    .get(7..11)
    .map_or(0, |us| u32::from_le_bytes([us[0], us[1], us[2], us[3]]));
  Ok(date.to_datetime(clock(hour, minute, second, micros)?))
}

/// A date or time field byte as jiff's `i8` (an out-of-range field is jiff's error to report).
fn small(field: u8) -> Result<i8, String> {
  i8::try_from(field).map_err(|_| format!("date-time field {field} is out of range"))
}

/// A wall-clock time from its fields (microsecond precision).
fn clock(hour: u8, minute: u8, second: u8, micros: u32) -> Result<Time, String> {
  let nanos = i32::try_from(micros)
    .ok()
    .filter(|_| micros < 1_000_000)
    .ok_or_else(|| format!("{micros} microseconds is beyond a second"))?;
  Time::new(small(hour)?, small(minute)?, small(second)?, nanos * 1_000).map_err(|e| e.to_string())
}

/// A `TIME` within a day as a time of day (negative or ≥ 24h is an error).
fn time_of_day(t: MySqlTime) -> Result<Time, String> {
  if !t.is_valid_time_of_day() {
    return Err(format!("TIME {t} is not a time of day"));
  }
  let hour = u8::try_from(t.hours()).map_err(|e| e.to_string())?;
  clock(hour, t.minutes(), t.seconds(), t.microseconds())
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
  fn binary_date_times_decode_every_length() {
    use jiff::civil::date;
    // 2024-02-29: year 2024 = 0x07e8.
    assert_eq!(
      datetime(&[4, 0xe8, 0x07, 2, 29]),
      Ok(date(2024, 2, 29).at(0, 0, 0, 0))
    );
    assert_eq!(
      datetime(&[7, 1, 0, 2, 3, 4, 5, 6]),
      Ok(date(1, 2, 3).at(4, 5, 6, 0))
    );
    // 999_999 µs = 0x000f423f.
    assert_eq!(
      datetime(&[11, 0x0f, 0x27, 12, 31, 23, 59, 59, 0x3f, 0x42, 0x0f, 0]),
      Ok(date(9999, 12, 31).at(23, 59, 59, 999_999_000))
    );
    assert_eq!(
      datetime(b"1999-12-31 23:59:59.000007"),
      Ok(date(1999, 12, 31).at(23, 59, 59, 7_000))
    );
    assert_eq!(datetime(b"0001-02-03"), Ok(date(1, 2, 3).at(0, 0, 0, 0)));
    assert_eq!(
      timestamp(date(1970, 1, 1).at(0, 0, 1, 1_000)),
      Ok(jiff::Timestamp::from_nanosecond(1_000_001_000).unwrap())
    );
  }

  #[test]
  fn malformed_date_times_are_errors_not_panics() {
    for bytes in [
      &[][..],
      &[4, 0xe8, 0x07, 2],
      &[5, 0xe8, 0x07, 2, 29, 0],
      &[4, 0xe8, 0x07, 2, 30],
      &[4, 0xe8, 0x07, 0, 0],
      &[4, 0xff, 0xff, 1, 1],
      &[4, 0x10, 0x27, 1, 1],
      &[7, 0xe8, 0x07, 1, 1, 24, 0, 0],
      &[7, 0xe8, 0x07, 1, 1, 200, 0, 0],
      &[11, 0xe8, 0x07, 1, 1, 0, 0, 0, 0x40, 0x42, 0x0f, 0],
      &[11, 0xe8, 0x07, 1, 1, 0, 0, 0, 0xff, 0xff, 0xff, 0xff],
      b"0000-00-00 00:00:00",
      b"2024-02-29 25:00:00",
    ] {
      assert!(datetime(bytes).is_err(), "{bytes:?}");
    }
    assert!(timestamp(jiff::civil::DateTime::MAX).is_err());
  }

  #[test]
  fn times_of_day_decode_within_a_day_only() {
    let t = |sign, h, m, s, us| MySqlTime::new(sign, h, m, s, us).unwrap();
    assert_eq!(
      time_of_day(t(MySqlTimeSign::Positive, 23, 59, 59, 999_999)),
      Ok(jiff::civil::time(23, 59, 59, 999_999_000))
    );
    assert!(
      time_of_day(t(MySqlTimeSign::Positive, 24, 0, 0, 0))
        .unwrap_err()
        .contains("not a time of day")
    );
    assert!(time_of_day(t(MySqlTimeSign::Negative, 1, 0, 0, 0)).is_err());
  }

  #[test]
  fn date_times_bind_as_literals_and_check_the_year() {
    use jiff::civil::{date, time};
    assert_eq!(date_literal(date(0, 1, 2)).as_deref(), Ok("0000-01-02"));
    assert_eq!(
      datetime_literal(date(2024, 2, 29).at(1, 2, 3, 456_789_999)).as_deref(),
      Ok("2024-02-29 01:02:03.456789")
    );
    assert_eq!(
      datetime_literal(date(9999, 12, 31).at(23, 59, 59, 0)).as_deref(),
      Ok("9999-12-31 23:59:59.000000")
    );
    let wire = time_to_wire(time(23, 59, 59, 999_999_999)).unwrap();
    assert_eq!(
      (
        wire.hours(),
        wire.minutes(),
        wire.seconds(),
        wire.microseconds()
      ),
      (23, 59, 59, 999_999)
    );
    assert!(year_in_range(date(0, 1, 1)).is_ok());
    assert!(
      datetime_literal(date(-1, 1, 1).at(0, 0, 0, 0))
        .unwrap_err()
        .contains("0000-9999")
    );
  }
}
