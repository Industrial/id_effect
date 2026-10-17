//! [`SqlValue`] ↔ SQLite: binding parameters and decoding result rows (see the
//! [module docs](super) for the mapping).
//!
//! sqlx reports a column's declared type only when it recognises it (`INTEGER`, `REAL`, `TEXT`,
//! `BLOB`, `BOOLEAN`, `DATE`, `TIME`, `DATETIME` / `TIMESTAMP`); anything else, and every
//! computed column, reads as `NULL`. Each value therefore decodes from its storage class, and
//! the declared type only refines INTEGER into `Bool` and TEXT into a date or time.

use std::sync::Arc;

use jiff::civil::{Date, DateTime, Time};
use sqlx::sqlite::{SqliteArguments, SqliteRow};
use sqlx::{Column as _, Row as _, Sqlite, TypeInfo as _, ValueRef as _};

use crate::{SqlColumn, SqlError, SqlRow, SqlType, SqlValue, ValueError};

pub(super) type SqliteQuery<'q> = sqlx::query::Query<'q, Sqlite, SqliteArguments<'q>>;

/// `query` with `params` bound to the placeholders (`?`, `?NNN`, `$NNN`) in order.
pub(super) fn bind_query<'q>(
  mut query: SqliteQuery<'q>,
  params: &[SqlValue],
) -> Result<SqliteQuery<'q>, SqlError> {
  for (i, param) in params.iter().enumerate() {
    let out_of_range = |message: String| SqlError::ValueOutOfRange {
      column: format!("${}", i + 1),
      message,
    };
    query = match param {
      SqlValue::Null => query.bind(None::<i64>),
      SqlValue::Bool(v) => query.bind(*v),
      SqlValue::I16(v) => query.bind(i64::from(*v)),
      SqlValue::I32(v) => query.bind(i64::from(*v)),
      SqlValue::I64(v) => query.bind(*v),
      SqlValue::F32(v) => query.bind(f64::from(*v)),
      SqlValue::F64(v) => query.bind(*v),
      SqlValue::Text(v) => query.bind(v.clone()),
      SqlValue::Bytes(v) => query.bind(v.clone()),
      SqlValue::Uuid(v) => query.bind(v.hyphenated().to_string()),
      SqlValue::Timestamp(v) => query.bind(timestamp_text(*v).map_err(out_of_range)?),
      SqlValue::DateTime(v) => query.bind(datetime_text(*v, ' ').map_err(out_of_range)?),
      SqlValue::Date(v) => query.bind(date_text(*v).map_err(out_of_range)?),
      SqlValue::Time(v) => query.bind(time_text(*v)),
      SqlValue::Decimal(v) => query.bind(v.to_string()),
      SqlValue::Json(v) => query.bind(v.to_string()),
    };
  }
  Ok(query)
}

fn date_text(d: Date) -> Result<String, String> {
  if d.year() < 0 {
    return Err(format!(
      "{d}: SQLite date-times are text in years 0000-9999"
    ));
  }
  Ok(format!("{:04}-{:02}-{:02}", d.year(), d.month(), d.day()))
}

/// `HH:MM:SS.ffffff` (sub-microsecond digits truncated).
fn time_text(t: Time) -> String {
  format!(
    "{:02}:{:02}:{:02}.{:06}",
    t.hour(),
    t.minute(),
    t.second(),
    t.subsec_nanosecond() / 1_000
  )
}

fn datetime_text(dt: DateTime, separator: char) -> Result<String, String> {
  Ok(format!(
    "{}{separator}{}",
    date_text(dt.date())?,
    time_text(dt.time())
  ))
}

/// `YYYY-MM-DDTHH:MM:SS.ffffffZ`, in UTC.
fn timestamp_text(t: jiff::Timestamp) -> Result<String, String> {
  datetime_text(jiff::tz::Offset::UTC.to_datetime(t), 'T').map(|text| text + "Z")
}

/// How a column's declared type (as sqlx reports it) refines its values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Declared {
  Integer,
  Real,
  Text,
  Blob,
  Bool,
  Date,
  Time,
  DateTime,
  /// No (recognised) declared type: a computed column, `NUMERIC`, `DECIMAL`, `UUID`, …
  Unknown,
}

impl Declared {
  fn of(name: &str) -> Self {
    match name {
      "INTEGER" => Self::Integer,
      "REAL" => Self::Real,
      "TEXT" => Self::Text,
      "BLOB" => Self::Blob,
      "BOOLEAN" => Self::Bool,
      "DATE" => Self::Date,
      "TIME" => Self::Time,
      "DATETIME" => Self::DateTime,
      _ => Self::Unknown,
    }
  }

  /// The portable type of a column whose values are all NULL.
  fn sql_type(self) -> SqlType {
    match self {
      Self::Integer => SqlType::I64,
      Self::Real => SqlType::F64,
      Self::Text => SqlType::Text,
      Self::Blob => SqlType::Bytes,
      Self::Bool => SqlType::Bool,
      Self::Date => SqlType::Date,
      Self::Time => SqlType::Time,
      Self::DateTime => SqlType::DateTime,
      Self::Unknown => SqlType::Null,
    }
  }
}

/// Decodes a result set: every value, then one shared column list (a column's type is its
/// first non-NULL value's, else its declared type's).
pub(super) fn decode_rows(rows: &[SqliteRow]) -> Result<Vec<SqlRow>, SqlError> {
  let Some(first) = rows.first() else {
    return Ok(Vec::new());
  };
  let declared: Vec<(&str, &str, Declared)> = first
    .columns()
    .iter()
    .map(|c| {
      let ty = c.type_info().name();
      (c.name(), ty, Declared::of(ty))
    })
    .collect();
  let values = rows
    .iter()
    .map(|row| {
      declared
        .iter()
        .enumerate()
        .map(|(i, (name, _, decl))| {
          decode(row, i, *decl).map_err(|e| SqlError::from_value_error(*name, e))
        })
        .collect::<Result<Vec<_>, _>>()
    })
    .collect::<Result<Vec<_>, _>>()?;
  let columns: Arc<[SqlColumn]> = declared
    .iter()
    .enumerate()
    .map(|(i, (name, ty, decl))| {
      let seen = values.iter().map(|row| &row[i]).find(|v| !v.is_null());
      let sql_type = seen.map_or(decl.sql_type(), SqlValue::sql_type);
      let db_type = match (*decl, seen) {
        (Declared::Unknown, Some(v)) => storage_class(v),
        _ => ty,
      };
      SqlColumn::new(*name, sql_type).with_db_type(db_type)
    })
    .collect();
  values
    .into_iter()
    .map(|values| SqlRow::from_parts(Arc::clone(&columns), values))
    .collect()
}

/// The storage class a decoded value came from (for a computed column's `db_type`).
fn storage_class(value: &SqlValue) -> &'static str {
  match value {
    SqlValue::I64(_) | SqlValue::Bool(_) => "INTEGER",
    SqlValue::F64(_) => "REAL",
    SqlValue::Bytes(_) => "BLOB",
    _ => "TEXT",
  }
}

fn invalid(e: impl std::fmt::Display) -> ValueError {
  ValueError::Invalid(e.to_string())
}

fn decode(row: &SqliteRow, i: usize, declared: Declared) -> Result<SqlValue, ValueError> {
  let raw = row.try_get_raw(i).map_err(invalid)?;
  if raw.is_null() {
    return Ok(SqlValue::Null);
  }
  let storage = raw.type_info().name().to_owned();
  Ok(match (storage.as_str(), declared) {
    ("INTEGER", Declared::Bool) => match row.try_get_unchecked::<i64, _>(i).map_err(invalid)? {
      0 => SqlValue::Bool(false),
      1 => SqlValue::Bool(true),
      n => {
        return Err(ValueError::OutOfRange(format!("BOOLEAN column holds {n}")));
      }
    },
    ("INTEGER", _) => SqlValue::I64(row.try_get_unchecked(i).map_err(invalid)?),
    ("REAL", _) => SqlValue::F64(row.try_get_unchecked(i).map_err(invalid)?),
    ("BLOB", _) => SqlValue::Bytes(row.try_get_unchecked(i).map_err(invalid)?),
    _ => {
      let text: String = row.try_get_unchecked(i).map_err(invalid)?;
      match declared {
        Declared::Date => SqlValue::Date(text.parse().map_err(invalid)?),
        Declared::Time => SqlValue::Time(text.parse().map_err(invalid)?),
        Declared::DateTime => datetime_value(&text)?,
        _ => SqlValue::Text(text),
      }
    }
  })
}

/// Text with an offset is an instant, text without one a wall-clock date-time.
fn datetime_value(text: &str) -> Result<SqlValue, ValueError> {
  if let Ok(t) = text.parse::<jiff::Timestamp>() {
    return Ok(SqlValue::Timestamp(t));
  }
  text.parse().map(SqlValue::DateTime).map_err(invalid)
}

#[cfg(test)]
mod tests {
  use jiff::civil::{date, time};

  use super::*;

  #[test]
  fn temporal_text_is_fixed_width_and_parses_back() {
    let t: jiff::Timestamp = "2024-02-29T12:34:56.1234567+01:00".parse().unwrap();
    let text = timestamp_text(t).unwrap();
    assert_eq!(text, "2024-02-29T11:34:56.123456Z");
    assert_eq!(
      datetime_value(&text),
      Ok(SqlValue::Timestamp(
        "2024-02-29T11:34:56.123456Z".parse().unwrap()
      ))
    );
    let local = date(5, 1, 2).at(3, 4, 5, 6_000);
    let text = datetime_text(local, ' ').unwrap();
    assert_eq!(text, "0005-01-02 03:04:05.000006");
    assert_eq!(datetime_value(&text), Ok(SqlValue::DateTime(local)));
    // SQLite's own CURRENT_TIMESTAMP format.
    assert_eq!(
      datetime_value("2024-01-01 00:00:00"),
      Ok(SqlValue::DateTime(date(2024, 1, 1).at(0, 0, 0, 0)))
    );
    assert!(matches!(
      datetime_value("yesterday"),
      Err(ValueError::Invalid(_))
    ));
    assert_eq!(time_text(time(23, 59, 59, 999_999_999)), "23:59:59.999999");
    assert_eq!(date_text(date(0, 1, 1)).unwrap(), "0000-01-01");
    assert!(date_text(date(-1, 1, 1)).unwrap_err().contains("0000-9999"));
  }

  #[test]
  fn declared_types_map_to_portable_types() {
    for (name, sql_type) in [
      ("INTEGER", SqlType::I64),
      ("REAL", SqlType::F64),
      ("TEXT", SqlType::Text),
      ("BLOB", SqlType::Bytes),
      ("BOOLEAN", SqlType::Bool),
      ("DATE", SqlType::Date),
      ("TIME", SqlType::Time),
      ("DATETIME", SqlType::DateTime),
      ("NULL", SqlType::Null),
      ("NUMERIC", SqlType::Null),
    ] {
      assert_eq!(Declared::of(name).sql_type(), sql_type, "{name}");
    }
    assert_eq!(storage_class(&SqlValue::Bool(true)), "INTEGER");
    assert_eq!(storage_class(&SqlValue::F64(1.0)), "REAL");
    assert_eq!(storage_class(&SqlValue::Bytes(vec![])), "BLOB");
    assert_eq!(storage_class(&SqlValue::Text("x".into())), "TEXT");
  }
}
