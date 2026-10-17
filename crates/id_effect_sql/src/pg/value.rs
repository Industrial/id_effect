//! [`SqlValue`] ↔ PostgreSQL: binding parameters and decoding result rows.
//!
//! Decoding switches on each column's type once per result set and shares one
//! `Arc<[SqlColumn]>` among its rows. Columns of a type outside [`SqlType`] (inet, interval,
//! arrays, ranges, …) are an error naming the column and suggesting a `::text` cast; enum
//! columns decode as text.
//!
//! Dates and times bind and decode through our own codecs for pg's binary form (microseconds /
//! days since 2000-01-01, see `datetime`) with checked arithmetic, so `±infinity`, values beyond
//! jiff's years ±9999 and values before pg's 4714 BC are [`SqlError::ValueOutOfRange`], never a
//! panic.

use std::sync::Arc;

use sqlx::decode::Decode;
use sqlx::encode::{Encode, IsNull};
use sqlx::error::BoxDynError;
use sqlx::postgres::types::Oid;
use sqlx::postgres::{PgArgumentBuffer, PgArguments, PgRow, PgTypeInfo, PgTypeKind, Postgres};
use sqlx::{Column as _, Row as _, TypeInfo as _, ValueRef as _};

use super::datetime::{PgDate, PgDateTime, PgTime, PgTimestamp};
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
      SqlValue::Timestamp(v) => query.bind(PgTimestamp::checked(*v).map_err(out_of_range)?),
      SqlValue::DateTime(v) => query.bind(PgDateTime::checked(*v).map_err(out_of_range)?),
      SqlValue::Date(v) => query.bind(PgDate::checked(*v).map_err(out_of_range)?),
      SqlValue::Time(v) => query.bind(PgTime(*v)),
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

/// Column `i` (not NULL) through `T`'s own `Decode` (the date-time codecs).
fn decode_as<T: for<'r> Decode<'r, Postgres>>(row: &PgRow, i: usize) -> Result<T, String> {
  T::decode(row.try_get_raw(i).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
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
    Kind::Timestamp => SqlValue::Timestamp(decode_as::<PgTimestamp>(row, i)?.0),
    Kind::DateTime => SqlValue::DateTime(decode_as::<PgDateTime>(row, i)?.0),
    Kind::Date => SqlValue::Date(decode_as::<PgDate>(row, i)?.0),
    Kind::Time => SqlValue::Time(decode_as::<PgTime>(row, i)?.0),
    Kind::Decimal => SqlValue::Decimal(row.try_get(i).map_err(err)?),
    Kind::Json => SqlValue::Json(row.try_get(i).map_err(err)?),
  })
}

#[cfg(test)]
mod tests {
  use super::*;

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
