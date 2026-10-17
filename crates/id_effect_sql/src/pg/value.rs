//! [`SqlValue`] ↔ PostgreSQL: binding parameters and decoding result rows.
//!
//! Decoding switches on each column's type once per result set and shares one
//! `Arc<[SqlColumn]>` among its rows. Columns of a type outside [`SqlType`] (inet, interval,
//! arrays, ranges, …) are an error naming the column and suggesting a `::text` cast; enum
//! columns decode as text.
//!
//! Dates and times are read from pg's binary form (microseconds / days since 2000-01-01) with
//! checked arithmetic, so `infinity` and values beyond jiff's years ±9999 are
//! [`SqlError::ValueOutOfRange`], never a panic.

use std::sync::Arc;

use sqlx::encode::{Encode, IsNull};
use sqlx::error::BoxDynError;
use sqlx::postgres::types::Oid;
use sqlx::postgres::{PgArgumentBuffer, PgArguments, PgRow, PgTypeInfo, PgTypeKind, Postgres};
use sqlx::{Column as _, Row as _, TypeInfo as _, ValueRef as _};

use crate::value::time_conv;
use crate::{SqlColumn, SqlError, SqlRow, SqlType, SqlValue};

/// SQL NULL bound with the unspecified type (OID 0): the server infers it from the statement.
struct UntypedNull;

impl sqlx::Type<Postgres> for UntypedNull {
  fn type_info() -> PgTypeInfo {
    PgTypeInfo::with_oid(Oid(0))
  }
}

impl Encode<'_, Postgres> for UntypedNull {
  fn encode_by_ref(&self, _buf: &mut PgArgumentBuffer) -> Result<IsNull, BoxDynError> {
    Ok(IsNull::Yes)
  }
}

pub(super) type PgQuery<'q> = sqlx::query::Query<'q, Postgres, PgArguments>;

/// `query` with `params` bound to `$1`, `$2`, … in order.
pub(super) fn bind_query<'q>(
  mut query: PgQuery<'q>,
  params: &[SqlValue],
) -> Result<PgQuery<'q>, SqlError> {
  for (i, param) in params.iter().enumerate() {
    let out_of_range = |message: String| SqlError::ValueOutOfRange {
      column: format!("${}", i + 1),
      message,
    };
    query = match param {
      SqlValue::Null => query.bind(UntypedNull),
      SqlValue::Bool(v) => query.bind(*v),
      SqlValue::I16(v) => query.bind(*v),
      SqlValue::I32(v) => query.bind(*v),
      SqlValue::I64(v) => query.bind(*v),
      SqlValue::F32(v) => query.bind(*v),
      SqlValue::F64(v) => query.bind(*v),
      SqlValue::Text(v) => query.bind(v.clone()),
      SqlValue::Bytes(v) => query.bind(v.clone()),
      SqlValue::Uuid(v) => query.bind(*v),
      SqlValue::Timestamp(v) => query.bind(time_conv::timestamp(*v).map_err(out_of_range)?),
      SqlValue::DateTime(v) => query.bind(time_conv::datetime(*v).map_err(out_of_range)?),
      SqlValue::Date(v) => query.bind(time_conv::date(*v).map_err(out_of_range)?),
      SqlValue::Time(v) => query.bind(time_conv::time_of_day(*v).map_err(out_of_range)?),
      SqlValue::Decimal(v) => query.bind(*v),
      SqlValue::Json(v) => query.bind(sqlx::types::Json(v.clone())),
    };
  }
  Ok(query)
}

/// How a column's non-NULL values decode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
  Bool,
  I16,
  I32,
  I64,
  Oid,
  F32,
  F64,
  Text,
  /// Text in a type sqlx's `String` does not declare (enums, citext).
  OtherText,
  Bytes,
  Uuid,
  Timestamp,
  DateTime,
  Date,
  Time,
  Decimal,
  Json,
}

/// The decoding and portable type of a pg column type, or `None` when it has none.
fn kind_of(ty: &PgTypeInfo) -> Option<(Kind, SqlType)> {
  Some(match ty.name() {
    "BOOL" => (Kind::Bool, SqlType::Bool),
    "INT2" => (Kind::I16, SqlType::I16),
    "INT4" => (Kind::I32, SqlType::I32),
    "INT8" => (Kind::I64, SqlType::I64),
    "OID" => (Kind::Oid, SqlType::I64),
    "FLOAT4" => (Kind::F32, SqlType::F32),
    "FLOAT8" => (Kind::F64, SqlType::F64),
    "TEXT" | "VARCHAR" | "BPCHAR" | "NAME" | "UNKNOWN" => (Kind::Text, SqlType::Text),
    "BYTEA" => (Kind::Bytes, SqlType::Bytes),
    "UUID" => (Kind::Uuid, SqlType::Uuid),
    "TIMESTAMPTZ" => (Kind::Timestamp, SqlType::Timestamp),
    "TIMESTAMP" => (Kind::DateTime, SqlType::DateTime),
    "DATE" => (Kind::Date, SqlType::Date),
    "TIME" => (Kind::Time, SqlType::Time),
    "NUMERIC" => (Kind::Decimal, SqlType::Decimal),
    "JSON" | "JSONB" => (Kind::Json, SqlType::Json),
    name if name.eq_ignore_ascii_case("citext") => (Kind::OtherText, SqlType::Text),
    // Result columns carry resolved types (`kind` is only defined for those, i.e. with an OID).
    _ if ty.oid().is_some() && matches!(ty.kind(), PgTypeKind::Enum(_)) => {
      (Kind::OtherText, SqlType::Text)
    }
    _ => return None,
  })
}

/// Decodes a result set: one shared column list, then every row.
pub(super) fn decode_rows(rows: &[PgRow]) -> Result<Vec<SqlRow>, SqlError> {
  let Some(first) = rows.first() else {
    return Ok(Vec::new());
  };
  let mut kinds = Vec::with_capacity(first.len());
  let mut columns = Vec::with_capacity(first.len());
  for column in first.columns() {
    let (name, ty) = (column.name(), column.type_info());
    let Some((kind, sql_type)) = kind_of(ty) else {
      return Err(SqlError::Unsupported(format!(
        "column `{name}` has type {}; cast it (`{name}::text`)",
        ty.name()
      )));
    };
    kinds.push(kind);
    columns.push(SqlColumn::new(name, sql_type).with_db_type(ty.name()));
  }
  let columns: Arc<[SqlColumn]> = columns.into();
  rows
    .iter()
    .map(|row| {
      let values = kinds
        .iter()
        .enumerate()
        .map(|(i, kind)| {
          decode(row, i, *kind).map_err(|message| SqlError::ValueOutOfRange {
            column: columns[i].name().to_owned(),
            message,
          })
        })
        .collect::<Result<Vec<_>, _>>()?;
      SqlRow::from_parts(Arc::clone(&columns), values)
    })
    .collect()
}

/// Microseconds from the unix epoch to pg's epoch, 2000-01-01T00:00:00Z.
const PG_EPOCH_MICROS: i64 = 946_684_800_000_000;
const DAY_MICROS: i64 = 86_400_000_000;

/// pg's binary timestamp (microseconds since its epoch) as an instant.
fn pg_timestamp(micros: i64) -> Result<jiff::Timestamp, String> {
  micros
    .checked_add(PG_EPOCH_MICROS)
    .ok_or_else(|| "timestamp is infinite or beyond year 9999".to_owned())
    .and_then(|unix| jiff::Timestamp::from_microsecond(unix).map_err(|e| e.to_string()))
}

fn pg_datetime(micros: i64) -> Result<jiff::civil::DateTime, String> {
  pg_timestamp(micros).map(|t| jiff::tz::Offset::UTC.to_datetime(t))
}

fn pg_date(days: i32) -> Result<jiff::civil::Date, String> {
  i64::from(days)
    .checked_mul(DAY_MICROS)
    .ok_or_else(|| "date is infinite or beyond year 9999".to_owned())
    .and_then(pg_datetime)
    .map(|dt| dt.date())
}

fn pg_time(micros: i64) -> Result<jiff::civil::Time, String> {
  if !(0..DAY_MICROS).contains(&micros) {
    return Err(format!("time of day {micros}µs is not before 24:00"));
  }
  pg_datetime(micros).map(|dt| dt.time())
}

fn decode(row: &PgRow, i: usize, kind: Kind) -> Result<SqlValue, String> {
  let err = |e: sqlx::Error| e.to_string();
  if row.try_get_raw(i).map_err(err)?.is_null() {
    return Ok(SqlValue::Null);
  }
  Ok(match kind {
    Kind::Bool => SqlValue::Bool(row.try_get(i).map_err(err)?),
    Kind::I16 => SqlValue::I16(row.try_get(i).map_err(err)?),
    Kind::I32 => SqlValue::I32(row.try_get(i).map_err(err)?),
    Kind::I64 => SqlValue::I64(row.try_get(i).map_err(err)?),
    Kind::Oid => SqlValue::I64(i64::from(row.try_get::<Oid, _>(i).map_err(err)?.0)),
    Kind::F32 => SqlValue::F32(row.try_get(i).map_err(err)?),
    Kind::F64 => SqlValue::F64(row.try_get(i).map_err(err)?),
    Kind::Text => SqlValue::Text(row.try_get(i).map_err(err)?),
    Kind::OtherText => SqlValue::Text(row.try_get_unchecked(i).map_err(err)?),
    Kind::Bytes => SqlValue::Bytes(row.try_get(i).map_err(err)?),
    Kind::Uuid => SqlValue::Uuid(row.try_get(i).map_err(err)?),
    Kind::Timestamp => SqlValue::Timestamp(pg_timestamp(row.try_get_unchecked(i).map_err(err)?)?),
    Kind::DateTime => SqlValue::DateTime(pg_datetime(row.try_get_unchecked(i).map_err(err)?)?),
    Kind::Date => SqlValue::Date(pg_date(row.try_get_unchecked(i).map_err(err)?)?),
    Kind::Time => SqlValue::Time(pg_time(row.try_get_unchecked(i).map_err(err)?)?),
    Kind::Decimal => SqlValue::Decimal(row.try_get(i).map_err(err)?),
    Kind::Json => SqlValue::Json(row.try_get(i).map_err(err)?),
  })
}

#[cfg(test)]
mod tests {
  use jiff::civil::{date, time};

  use super::*;

  #[test]
  fn binary_temporals_decode_with_checked_arithmetic() {
    assert_eq!(
      pg_timestamp(0).unwrap(),
      "2000-01-01T00:00:00Z".parse::<jiff::Timestamp>().unwrap()
    );
    assert_eq!(
      pg_datetime(-1).unwrap(),
      date(1999, 12, 31).at(23, 59, 59, 999_999_000)
    );
    assert_eq!(pg_date(366).unwrap(), date(2001, 1, 1));
    assert_eq!(pg_time(3_600_000_001).unwrap(), time(1, 0, 0, 1_000));
    assert!(pg_timestamp(i64::MAX).unwrap_err().contains("infinite"));
    assert!(pg_timestamp(i64::MIN).is_err(), "-infinity");
    assert!(pg_date(i32::MAX).is_err(), "infinity date");
    assert!(pg_time(DAY_MICROS).unwrap_err().contains("24:00"));
    assert!(pg_time(-1).is_err());
  }

  #[test]
  fn type_names_map_to_kinds() {
    for (name, sql_type) in [
      ("INT8", SqlType::I64),
      ("OID", SqlType::I64),
      ("VARCHAR", SqlType::Text),
      ("JSONB", SqlType::Json),
      ("TIMESTAMP", SqlType::DateTime),
      ("TIMESTAMPTZ", SqlType::Timestamp),
    ] {
      let ty = PgTypeInfo::with_name(name);
      assert_eq!(kind_of(&ty).map(|k| k.1), Some(sql_type), "{name}");
    }
    assert_eq!(kind_of(&PgTypeInfo::with_name("INET")), None);
  }
}
