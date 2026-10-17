//! [`ToSqlValue`] / [`FromSqlValue`] / `From<T> for SqlValue` for std and value-crate types.

use std::str::FromStr;
use std::sync::Arc;

use jiff::civil::{Date, DateTime, Time};
use rust_decimal::Decimal;
use serde::de::DeserializeOwned;
use uuid::Uuid;

use super::{FromSqlValue, SqlType, SqlValue, ToSqlValue, ValueError};

/// A JSON column decoded into `T` with serde (`Json` or parseable `Text` values). Encode a value
/// with [`SqlValue::json`].
///
/// ```rust
/// use id_effect_sql::{FromSqlValue, Json, SqlValue};
///
/// #[derive(serde::Deserialize, serde::Serialize, Debug, PartialEq)]
/// struct Tags { names: Vec<String> }
///
/// let v = SqlValue::json(&Tags { names: vec!["a".into()] }).unwrap();
/// let Json(tags) = Json::<Tags>::from_sql_value(&v).unwrap();
/// assert_eq!(tags.names, ["a"]);
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Json<T>(pub T);

/// `UnexpectedNull` for a NULL, `Mismatch` otherwise.
fn mismatch(expected: SqlType, v: &SqlValue) -> ValueError {
  if v.is_null() {
    ValueError::UnexpectedNull
  } else {
    ValueError::Mismatch {
      expected,
      found: v.sql_type(),
    }
  }
}

fn invalid(text: &str, what: &str, err: impl std::fmt::Display) -> ValueError {
  ValueError::Invalid(format!("`{text}` is not a {what}: {err}"))
}

// ---- encoding -----------------------------------------------------------------------------

macro_rules! same_variant {
  ($($ty:ty => $variant:ident),* $(,)?) => {$(
    impl From<$ty> for SqlValue {
      #[inline]
      fn from(v: $ty) -> Self {
        SqlValue::$variant(v)
      }
    }

    impl ToSqlValue for $ty {
      #[inline]
      #[allow(clippy::clone_on_copy)]
      fn to_sql_value(&self) -> SqlValue {
        SqlValue::$variant(self.clone())
      }
    }
  )*};
}

same_variant!(
  bool => Bool,
  i16 => I16,
  i32 => I32,
  i64 => I64,
  f32 => F32,
  f64 => F64,
  String => Text,
  Vec<u8> => Bytes,
  Uuid => Uuid,
  jiff::Timestamp => Timestamp,
  DateTime => DateTime,
  Date => Date,
  Time => Time,
  Decimal => Decimal,
  serde_json::Value => Json,
);

macro_rules! widened {
  ($($ty:ty => $variant:ident($wide:ty)),* $(,)?) => {$(
    impl From<$ty> for SqlValue {
      #[inline]
      fn from(v: $ty) -> Self {
        SqlValue::$variant(<$wide>::from(v))
      }
    }

    impl ToSqlValue for $ty {
      #[inline]
      fn to_sql_value(&self) -> SqlValue {
        SqlValue::from(*self)
      }
    }
  )*};
}

widened!(u8 => I16(i16), u16 => I32(i32), u32 => I64(i64));

impl From<&str> for SqlValue {
  #[inline]
  fn from(v: &str) -> Self {
    SqlValue::Text(v.to_owned())
  }
}

impl From<Box<str>> for SqlValue {
  #[inline]
  fn from(v: Box<str>) -> Self {
    SqlValue::Text(v.into())
  }
}

impl From<Arc<str>> for SqlValue {
  #[inline]
  fn from(v: Arc<str>) -> Self {
    SqlValue::Text(v.as_ref().to_owned())
  }
}

impl From<&[u8]> for SqlValue {
  #[inline]
  fn from(v: &[u8]) -> Self {
    SqlValue::Bytes(v.to_vec())
  }
}

impl<T: Into<SqlValue>> From<Option<T>> for SqlValue {
  #[inline]
  fn from(v: Option<T>) -> Self {
    v.map_or(SqlValue::Null, Into::into)
  }
}

impl ToSqlValue for str {
  #[inline]
  fn to_sql_value(&self) -> SqlValue {
    SqlValue::Text(self.to_owned())
  }
}

impl ToSqlValue for Box<str> {
  #[inline]
  fn to_sql_value(&self) -> SqlValue {
    SqlValue::Text(self.as_ref().to_owned())
  }
}

impl ToSqlValue for Arc<str> {
  #[inline]
  fn to_sql_value(&self) -> SqlValue {
    SqlValue::Text(self.as_ref().to_owned())
  }
}

impl ToSqlValue for [u8] {
  #[inline]
  fn to_sql_value(&self) -> SqlValue {
    SqlValue::Bytes(self.to_vec())
  }
}

impl ToSqlValue for SqlValue {
  #[inline]
  fn to_sql_value(&self) -> SqlValue {
    self.clone()
  }
}

impl<T: ToSqlValue + ?Sized> ToSqlValue for &T {
  #[inline]
  fn to_sql_value(&self) -> SqlValue {
    (**self).to_sql_value()
  }
}

impl<T: ToSqlValue> ToSqlValue for Option<T> {
  #[inline]
  fn to_sql_value(&self) -> SqlValue {
    self
      .as_ref()
      .map_or(SqlValue::Null, ToSqlValue::to_sql_value)
  }
}

// ---- decoding -----------------------------------------------------------------------------

impl FromSqlValue for SqlValue {
  #[inline]
  fn from_sql_value(v: &SqlValue) -> Result<Self, ValueError> {
    Ok(v.clone())
  }
}

impl<T: FromSqlValue> FromSqlValue for Option<T> {
  fn from_sql_value(v: &SqlValue) -> Result<Self, ValueError> {
    match v {
      SqlValue::Null => Ok(None),
      other => T::from_sql_value(other).map(Some),
    }
  }
}

impl FromSqlValue for bool {
  fn from_sql_value(v: &SqlValue) -> Result<Self, ValueError> {
    match v {
      SqlValue::Bool(b) => Ok(*b),
      other => Err(mismatch(SqlType::Bool, other)),
    }
  }
}

/// Any integer variant, widened.
fn integer(v: &SqlValue) -> Option<i64> {
  match v {
    SqlValue::I16(n) => Some(i64::from(*n)),
    SqlValue::I32(n) => Some(i64::from(*n)),
    SqlValue::I64(n) => Some(*n),
    _ => None,
  }
}

macro_rules! integers {
  ($($ty:ty => $expected:ident),* $(,)?) => {$(
    impl FromSqlValue for $ty {
      #[allow(clippy::useless_conversion)]
      fn from_sql_value(v: &SqlValue) -> Result<Self, ValueError> {
        let n = integer(v).ok_or_else(|| mismatch(SqlType::$expected, v))?;
        <$ty>::try_from(n).map_err(|_| {
          ValueError::OutOfRange(format!("{n} does not fit in {}", stringify!($ty)))
        })
      }
    }
  )*};
}

integers!(
  i16 => I16,
  i32 => I32,
  i64 => I64,
  u8 => I16,
  u16 => I32,
  u32 => I64,
  u64 => I64,
);

impl FromSqlValue for f64 {
  fn from_sql_value(v: &SqlValue) -> Result<Self, ValueError> {
    match v {
      SqlValue::F64(x) => Ok(*x),
      SqlValue::F32(x) => Ok(f64::from(*x)),
      other => Err(mismatch(SqlType::F64, other)),
    }
  }
}

impl FromSqlValue for f32 {
  #[allow(clippy::cast_possible_truncation)]
  fn from_sql_value(v: &SqlValue) -> Result<Self, ValueError> {
    match v {
      SqlValue::F32(x) => Ok(*x),
      SqlValue::F64(x) => {
        let narrow = *x as f32;
        if f64::from(narrow) == *x || x.is_nan() {
          Ok(narrow)
        } else {
          Err(ValueError::OutOfRange(format!(
            "{x} is not exactly representable as f32"
          )))
        }
      }
      other => Err(mismatch(SqlType::F32, other)),
    }
  }
}

fn text(v: &SqlValue) -> Result<&str, ValueError> {
  match v {
    SqlValue::Text(s) => Ok(s),
    other => Err(mismatch(SqlType::Text, other)),
  }
}

impl FromSqlValue for String {
  fn from_sql_value(v: &SqlValue) -> Result<Self, ValueError> {
    text(v).map(str::to_owned)
  }
}

impl FromSqlValue for Box<str> {
  fn from_sql_value(v: &SqlValue) -> Result<Self, ValueError> {
    text(v).map(Box::from)
  }
}

impl FromSqlValue for Arc<str> {
  fn from_sql_value(v: &SqlValue) -> Result<Self, ValueError> {
    text(v).map(Arc::from)
  }
}

impl FromSqlValue for Vec<u8> {
  fn from_sql_value(v: &SqlValue) -> Result<Self, ValueError> {
    match v {
      SqlValue::Bytes(b) => Ok(b.clone()),
      other => Err(mismatch(SqlType::Bytes, other)),
    }
  }
}

impl FromSqlValue for Uuid {
  fn from_sql_value(v: &SqlValue) -> Result<Self, ValueError> {
    match v {
      SqlValue::Uuid(u) => Ok(*u),
      SqlValue::Bytes(b) => Uuid::from_slice(b)
        .map_err(|_| ValueError::Invalid(format!("{} bytes are not a uuid", b.len()))),
      SqlValue::Text(s) => Uuid::parse_str(s).map_err(|e| invalid(s, "uuid", e)),
      other => Err(mismatch(SqlType::Uuid, other)),
    }
  }
}

/// The same-name variant, or `Text` parsed with `FromStr`.
macro_rules! parsed {
  ($($ty:ty => $variant:ident, $what:literal);* $(;)?) => {$(
    impl FromSqlValue for $ty {
      fn from_sql_value(v: &SqlValue) -> Result<Self, ValueError> {
        match v {
          SqlValue::$variant(x) => Ok(*x),
          SqlValue::Text(s) => s.parse::<$ty>().map_err(|e| invalid(s, $what, e)),
          other => Err(mismatch(SqlType::$variant, other)),
        }
      }
    }
  )*};
}

parsed!(
  jiff::Timestamp => Timestamp, "timestamp";
  DateTime => DateTime, "date-time";
  Date => Date, "date";
  Time => Time, "time";
);

impl FromSqlValue for Decimal {
  fn from_sql_value(v: &SqlValue) -> Result<Self, ValueError> {
    match v {
      SqlValue::Decimal(d) => Ok(*d),
      SqlValue::Text(s) => Decimal::from_str(s).map_err(|e| invalid(s, "decimal", e)),
      other => integer(other)
        .map(Decimal::from)
        .ok_or_else(|| mismatch(SqlType::Decimal, other)),
    }
  }
}

impl FromSqlValue for serde_json::Value {
  fn from_sql_value(v: &SqlValue) -> Result<Self, ValueError> {
    match v {
      SqlValue::Json(j) => Ok(j.clone()),
      SqlValue::Text(s) => serde_json::from_str(s).map_err(|e| invalid(s, "JSON document", e)),
      other => Err(mismatch(SqlType::Json, other)),
    }
  }
}

impl<T: DeserializeOwned> FromSqlValue for Json<T> {
  fn from_sql_value(v: &SqlValue) -> Result<Self, ValueError> {
    let decoded = match v {
      SqlValue::Json(j) => serde_json::from_value(j.clone()),
      SqlValue::Text(s) => serde_json::from_str(s),
      other => return Err(mismatch(SqlType::Json, other)),
    };
    decoded
      .map(Json)
      .map_err(|e| ValueError::Invalid(format!("JSON of the wrong shape: {e}")))
  }
}

#[cfg(test)]
mod tests {
  use std::fmt::Debug;
  use std::sync::Arc;

  use jiff::civil::{date, time};
  use proptest::prelude::*;
  use rust_decimal::Decimal;
  use serde_json::json;
  use uuid::Uuid;

  use super::Json;
  use crate::params;
  use crate::value::{FromSqlValue, SqlType, SqlValue, ToSqlValue, ValueError};

  /// `t` encodes to `expected` (both ways) and decodes back.
  fn both_ways<T>(t: T, expected: SqlValue)
  where
    T: ToSqlValue + FromSqlValue + Into<SqlValue> + Clone + PartialEq + Debug,
  {
    assert_eq!(t.to_sql_value(), expected);
    assert_eq!(t.clone().into(), expected);
    assert_eq!(T::from_sql_value(&expected), Ok(t));
  }

  fn mismatch(expected: SqlType, found: SqlType) -> ValueError {
    ValueError::Mismatch { expected, found }
  }

  #[test]
  fn every_table_row_encodes_and_decodes() {
    both_ways(true, SqlValue::Bool(true));
    both_ways(-3i16, SqlValue::I16(-3));
    both_ways(4i32, SqlValue::I32(4));
    both_ways(5i64, SqlValue::I64(5));
    both_ways(6u8, SqlValue::I16(6));
    both_ways(7u16, SqlValue::I32(7));
    both_ways(8u32, SqlValue::I64(8));
    both_ways(1.5f32, SqlValue::F32(1.5));
    both_ways(2.5f64, SqlValue::F64(2.5));
    both_ways(String::from("s"), SqlValue::Text("s".into()));
    both_ways(Box::<str>::from("b"), SqlValue::Text("b".into()));
    both_ways(Arc::<str>::from("a"), SqlValue::Text("a".into()));
    both_ways(vec![1u8, 2], SqlValue::Bytes(vec![1, 2]));
    let id = Uuid::from_u128(0x1234);
    both_ways(id, SqlValue::Uuid(id));
    let ts = jiff::Timestamp::from_second(1_700_000_000).unwrap();
    both_ways(ts, SqlValue::Timestamp(ts));
    let dt = date(2024, 5, 6).at(7, 8, 9, 0);
    both_ways(dt, SqlValue::DateTime(dt));
    both_ways(date(2024, 5, 6), SqlValue::Date(date(2024, 5, 6)));
    both_ways(time(7, 8, 9, 0), SqlValue::Time(time(7, 8, 9, 0)));
    both_ways(
      Decimal::new(105, 1),
      SqlValue::Decimal(Decimal::new(105, 1)),
    );
    both_ways(json!({"k": 1}), SqlValue::Json(json!({"k": 1})));
    both_ways(Some(1i64), SqlValue::I64(1));
    both_ways(None::<i64>, SqlValue::Null);
    both_ways(SqlValue::Text("x".into()), SqlValue::Text("x".into()));
    assert_eq!("s".to_sql_value(), SqlValue::Text("s".into()));
    assert_eq!(SqlValue::from("s"), SqlValue::Text("s".into()));
    assert_eq!([1u8].as_slice().to_sql_value(), SqlValue::Bytes(vec![1]));
    assert_eq!(SqlValue::from([1u8].as_slice()), SqlValue::Bytes(vec![1]));
    assert_eq!(SqlValue::from(Some("s")), SqlValue::Text("s".into()));
    assert_eq!(u64::from_sql_value(&SqlValue::I64(9)), Ok(9));
  }

  #[test]
  fn params_macro_encodes_by_reference() {
    let title = String::from("t");
    let p = params![1i64, title, &title, None::<Uuid>, "x"];
    assert_eq!(p.len(), 5);
    assert_eq!(p[1], p[2]);
    assert_eq!(p[3], SqlValue::Null);
    assert_eq!(title, "t", "params! does not move its arguments");
  }

  #[test]
  fn integers_are_range_checked_across_widths() {
    assert_eq!(i16::from_sql_value(&SqlValue::I64(-7)), Ok(-7));
    assert_eq!(i64::from_sql_value(&SqlValue::I16(7)), Ok(7));
    assert_eq!(i32::from_sql_value(&SqlValue::I32(7)), Ok(7));
    assert!(matches!(
      i32::from_sql_value(&SqlValue::I64(i64::MAX)),
      Err(ValueError::OutOfRange(m)) if m.contains("i32")
    ));
    assert!(matches!(
      u64::from_sql_value(&SqlValue::I64(-1)),
      Err(ValueError::OutOfRange(_))
    ));
    assert!(matches!(
      u8::from_sql_value(&SqlValue::I16(300)),
      Err(ValueError::OutOfRange(_))
    ));
    assert_eq!(
      i64::from_sql_value(&SqlValue::Text("1".into())),
      Err(mismatch(SqlType::I64, SqlType::Text)),
      "text is never parsed into numbers"
    );
    assert_eq!(
      i64::from_sql_value(&SqlValue::Null),
      Err(ValueError::UnexpectedNull)
    );
  }

  #[test]
  fn floats_widen_and_narrow_only_exactly() {
    assert_eq!(f64::from_sql_value(&SqlValue::F32(0.5)), Ok(0.5));
    assert_eq!(f32::from_sql_value(&SqlValue::F64(0.5)), Ok(0.5));
    assert!(
      f32::from_sql_value(&SqlValue::F64(f64::NAN))
        .unwrap()
        .is_nan()
    );
    assert!(matches!(
      f32::from_sql_value(&SqlValue::F64(0.1)),
      Err(ValueError::OutOfRange(_))
    ));
    assert_eq!(
      f64::from_sql_value(&SqlValue::I64(1)),
      Err(mismatch(SqlType::F64, SqlType::I64)),
      "no int to float"
    );
    assert_eq!(
      f32::from_sql_value(&SqlValue::I32(1)),
      Err(mismatch(SqlType::F32, SqlType::I32))
    );
  }

  #[test]
  fn strict_types_reject_other_variants() {
    assert_eq!(
      bool::from_sql_value(&SqlValue::I64(1)),
      Err(mismatch(SqlType::Bool, SqlType::I64))
    );
    assert_eq!(
      String::from_sql_value(&SqlValue::I64(1)),
      Err(mismatch(SqlType::Text, SqlType::I64))
    );
    assert_eq!(
      Vec::<u8>::from_sql_value(&SqlValue::Text("x".into())),
      Err(mismatch(SqlType::Bytes, SqlType::Text))
    );
    assert_eq!(
      Uuid::from_sql_value(&SqlValue::I64(1)),
      Err(mismatch(SqlType::Uuid, SqlType::I64))
    );
    assert_eq!(
      jiff::Timestamp::from_sql_value(&SqlValue::I64(1)),
      Err(mismatch(SqlType::Timestamp, SqlType::I64))
    );
    assert_eq!(
      Decimal::from_sql_value(&SqlValue::F64(1.0)),
      Err(mismatch(SqlType::Decimal, SqlType::F64))
    );
    assert_eq!(
      serde_json::Value::from_sql_value(&SqlValue::Bool(true)),
      Err(mismatch(SqlType::Json, SqlType::Bool))
    );
    assert_eq!(
      Json::<i32>::from_sql_value(&SqlValue::Null),
      Err(ValueError::UnexpectedNull)
    );
    assert_eq!(
      Option::<bool>::from_sql_value(&SqlValue::I64(1)),
      Err(mismatch(SqlType::Bool, SqlType::I64))
    );
  }

  #[test]
  fn text_fallbacks_parse_uuid_temporal_decimal_and_json() {
    let id = Uuid::from_u128(42);
    assert_eq!(
      Uuid::from_sql_value(&SqlValue::Text(id.to_string())),
      Ok(id)
    );
    assert_eq!(
      Uuid::from_sql_value(&SqlValue::Bytes(id.as_bytes().to_vec())),
      Ok(id)
    );
    assert!(matches!(
      Uuid::from_sql_value(&SqlValue::Bytes(vec![1, 2])),
      Err(ValueError::Invalid(m)) if m.contains("2 bytes")
    ));
    assert!(matches!(
      Uuid::from_sql_value(&SqlValue::Text("nope".into())),
      Err(ValueError::Invalid(m)) if m.contains("`nope` is not a uuid")
    ));
    assert_eq!(
      jiff::Timestamp::from_sql_value(&SqlValue::Text("2024-01-02T03:04:05+01:00".into())),
      Ok("2024-01-02T02:04:05Z".parse().unwrap())
    );
    assert!(matches!(
      jiff::Timestamp::from_sql_value(&SqlValue::Text("2024-01-02T03:04:05".into())),
      Err(ValueError::Invalid(_))
    ));
    assert_eq!(
      jiff::civil::DateTime::from_sql_value(&SqlValue::Text("2024-01-02T03:04:05".into())),
      Ok(date(2024, 1, 2).at(3, 4, 5, 0))
    );
    assert_eq!(
      jiff::civil::Date::from_sql_value(&SqlValue::Text("2024-01-02".into())),
      Ok(date(2024, 1, 2))
    );
    assert_eq!(
      jiff::civil::Time::from_sql_value(&SqlValue::Text("03:04:05.5".into())),
      Ok(time(3, 4, 5, 500_000_000))
    );
    assert_eq!(
      Decimal::from_sql_value(&SqlValue::Text("10.25".into())),
      Ok(Decimal::new(1025, 2))
    );
    assert_eq!(
      Decimal::from_sql_value(&SqlValue::I32(3)),
      Ok(Decimal::from(3))
    );
    assert!(matches!(
      Decimal::from_sql_value(&SqlValue::Text("ten".into())),
      Err(ValueError::Invalid(_))
    ));
    assert_eq!(
      serde_json::Value::from_sql_value(&SqlValue::Text("[1]".into())),
      Ok(json!([1]))
    );
    assert!(matches!(
      serde_json::Value::from_sql_value(&SqlValue::Text("{".into())),
      Err(ValueError::Invalid(_))
    ));
  }

  #[test]
  fn typed_json_decodes_with_serde() {
    #[derive(serde::Deserialize, Debug, PartialEq)]
    struct Point {
      x: i32,
    }
    assert_eq!(
      Json::<Point>::from_sql_value(&SqlValue::Json(json!({"x": 1}))),
      Ok(Json(Point { x: 1 }))
    );
    assert_eq!(
      Json::<Point>::from_sql_value(&SqlValue::Text(r#"{"x": 2}"#.into())),
      Ok(Json(Point { x: 2 }))
    );
    assert!(matches!(
      Json::<Point>::from_sql_value(&SqlValue::Json(json!({"y": 1}))),
      Err(ValueError::Invalid(m)) if m.contains("wrong shape")
    ));
    assert_eq!(
      Json::<Point>::from_sql_value(&SqlValue::I64(1)),
      Err(mismatch(SqlType::Json, SqlType::I64))
    );
  }

  fn round_trips<T>(t: T) -> Result<(), TestCaseError>
  where
    T: ToSqlValue + FromSqlValue + PartialEq + Debug,
  {
    prop_assert_eq!(T::from_sql_value(&t.to_sql_value()), Ok(t));
    Ok(())
  }

  proptest! {
    #[test]
    fn prop_integers_and_bools_round_trip(
      b in any::<bool>(), a in any::<i16>(), c in any::<i32>(), d in any::<i64>(),
      e in any::<u8>(), f in any::<u16>(), g in any::<u32>(),
    ) {
      round_trips(b)?;
      round_trips(a)?;
      round_trips(c)?;
      round_trips(d)?;
      round_trips(e)?;
      round_trips(f)?;
      round_trips(g)?;
      round_trips(Some(d))?;
    }

    #[test]
    fn prop_floats_text_and_bytes_round_trip(
      x in any::<f32>().prop_filter("finite", |x| x.is_finite()),
      y in any::<f64>().prop_filter("finite", |y| y.is_finite()),
      s in ".*",
      bytes in proptest::collection::vec(any::<u8>(), 0..32),
    ) {
      round_trips(x)?;
      round_trips(y)?;
      round_trips(s)?;
      round_trips(bytes)?;
    }

    #[test]
    fn prop_value_crates_round_trip(
      id in any::<u128>(),
      secs in -62_135_596_800i64..253_402_300_000, // years 1..=9999
      nanos in 0i32..1_000_000_000,
      mantissa in any::<i64>(),
      scale in 0u32..=28,
    ) {
      round_trips(Uuid::from_u128(id))?;
      let ts = jiff::Timestamp::new(secs, nanos).unwrap();
      round_trips(ts)?;
      let civil = ts.to_zoned(jiff::tz::TimeZone::UTC).datetime();
      round_trips(civil)?;
      round_trips(civil.date())?;
      round_trips(civil.time())?;
      round_trips(Decimal::new(mantissa, scale))?;
      round_trips(serde_json::json!({"n": mantissa, "s": scale}))?;
      // Text forms parse back to the same value.
      prop_assert_eq!(jiff::Timestamp::from_sql_value(&SqlValue::Text(ts.to_string())), Ok(ts));
      prop_assert_eq!(Uuid::from_sql_value(&SqlValue::Text(Uuid::from_u128(id).to_string())), Ok(Uuid::from_u128(id)));
      let d = Decimal::new(mantissa, scale);
      prop_assert_eq!(Decimal::from_sql_value(&SqlValue::Text(d.to_string())), Ok(d));
    }
  }
}
