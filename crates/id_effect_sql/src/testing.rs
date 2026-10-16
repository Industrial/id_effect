//! proptest strategies for [`SqlValue`] (feature `proptest`), built on
//! [`id_effect::testing::property`].
//!
//! The generators stay inside the domain every driver round-trips exactly: finite floats, text
//! without `\0`, dates and times in years 1..=9999 at microsecond precision, decimals of at most
//! 19 digits, JSON whose numbers are `i32`s.
//!
//! ```rust
//! use id_effect_sql::testing::arb_sql_value;
//! use id_effect_sql::{FromSqlValue, SqlValue};
//! use proptest::prelude::*;
//!
//! proptest!(|(v in arb_sql_value())| {
//!   prop_assert_eq!(SqlValue::from_sql_value(&v), Ok(v.clone()));
//! });
//! ```

use id_effect::schema::Unknown;
use id_effect::testing::property::arb_unknown;
use proptest::collection::vec;
use proptest::prelude::{Just, Strategy, any};
use proptest::prop_oneof;

use crate::value::SqlValue;

/// Any portable [`SqlValue`], every variant (including [`SqlValue::Null`]).
pub fn arb_sql_value() -> impl Strategy<Value = SqlValue> {
  prop_oneof![
    Just(SqlValue::Null),
    any::<bool>().prop_map(SqlValue::Bool),
    any::<i16>().prop_map(SqlValue::I16),
    any::<i32>().prop_map(SqlValue::I32),
    any::<i64>().prop_map(SqlValue::I64),
    any::<f32>()
      .prop_filter("finite", |x| x.is_finite())
      .prop_map(SqlValue::F32),
    any::<f64>()
      .prop_filter("finite", |x| x.is_finite())
      .prop_map(SqlValue::F64),
    any::<String>().prop_map(|s| SqlValue::Text(s.replace('\0', ""))),
    vec(any::<u8>(), 0..32).prop_map(SqlValue::Bytes),
    any::<u128>().prop_map(|n| SqlValue::Uuid(uuid::Uuid::from_u128(n))),
    arb_timestamp().prop_map(SqlValue::Timestamp),
    arb_timestamp().prop_map(|t| SqlValue::DateTime(utc(t))),
    arb_timestamp().prop_map(|t| SqlValue::Date(utc(t).date())),
    arb_timestamp().prop_map(|t| SqlValue::Time(utc(t).time())),
    (any::<i64>(), 0u32..=18)
      .prop_map(|(m, scale)| SqlValue::Decimal(rust_decimal::Decimal::new(m, scale))),
    arb_unknown().prop_map(|u| SqlValue::Json(json(u))),
  ]
}

/// Instants in years 1..=9999, truncated to microseconds.
fn arb_timestamp() -> impl Strategy<Value = jiff::Timestamp> {
  (-62_135_596_800i64..253_402_300_000, 0i32..1_000_000).prop_map(|(secs, micros)| {
    jiff::Timestamp::new(secs, micros * 1_000).expect("generated in jiff's range")
  })
}

fn utc(t: jiff::Timestamp) -> jiff::civil::DateTime {
  jiff::tz::Offset::UTC.to_datetime(t)
}

/// A JSON tree with every number narrowed to an `i32`.
fn json(u: Unknown) -> serde_json::Value {
  match u {
    Unknown::Null => serde_json::Value::Null,
    Unknown::Bool(b) => b.into(),
    Unknown::I64(n) => (n % i64::from(i32::MAX)).into(),
    Unknown::F64(x) => (x as i32).into(),
    Unknown::String(s) => s.into(),
    Unknown::Array(items) => items.into_iter().map(json).collect(),
    Unknown::Object(fields) => fields.into_iter().map(|(k, v)| (k, json(v))).collect(),
  }
}

#[cfg(test)]
mod tests {
  use jiff::civil::{Date, DateTime, Time};
  use proptest::prelude::*;
  use rust_decimal::Decimal;

  use super::arb_sql_value;
  use crate::value::{FromSqlValue, SqlValue, ToSqlValue};

  fn typed<T: FromSqlValue + ToSqlValue>(v: &SqlValue) -> Result<(), TestCaseError> {
    let t = T::from_sql_value(v).map_err(|e| TestCaseError::fail(e.to_string()))?;
    prop_assert_eq!(&t.to_sql_value(), v);
    Ok(())
  }

  proptest! {
    #[test]
    fn prop_every_value_round_trips_through_its_rust_type(v in arb_sql_value()) {
      prop_assert_eq!(SqlValue::from_sql_value(&v), Ok(v.clone()));
      let _ = v.to_unknown();
      match &v {
        SqlValue::Null => typed::<Option<i64>>(&v)?,
        SqlValue::Bool(_) => typed::<bool>(&v)?,
        SqlValue::I16(_) => typed::<i16>(&v)?,
        SqlValue::I32(_) => typed::<i32>(&v)?,
        SqlValue::I64(_) => typed::<i64>(&v)?,
        SqlValue::F32(_) => typed::<f32>(&v)?,
        SqlValue::F64(_) => typed::<f64>(&v)?,
        SqlValue::Text(s) => {
          prop_assert!(!s.contains('\0'));
          typed::<String>(&v)?;
        }
        SqlValue::Bytes(_) => typed::<Vec<u8>>(&v)?,
        SqlValue::Uuid(_) => typed::<uuid::Uuid>(&v)?,
        SqlValue::Timestamp(t) => {
          prop_assert_eq!(t.subsec_nanosecond() % 1_000, 0);
          typed::<jiff::Timestamp>(&v)?;
        }
        SqlValue::DateTime(_) => typed::<DateTime>(&v)?,
        SqlValue::Date(_) => typed::<Date>(&v)?,
        SqlValue::Time(_) => typed::<Time>(&v)?,
        SqlValue::Decimal(_) => typed::<Decimal>(&v)?,
        SqlValue::Json(_) => typed::<serde_json::Value>(&v)?,
      }
    }
  }
}
