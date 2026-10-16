//! Convert [`serde_json::Value`] to [`crate::schema::parse::Unknown`] for schema decoding.
//!
//! Enabled with the **`schema-serde`** crate feature. HTTP adapters (`id_effect_axum`, `id_effect_platform`)
//! should use this instead of duplicating conversion logic.
//!
//! See [`TESTING.md`](../../../../TESTING.md) for validation policy.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::schema::parse::Unknown;

/// Map JSON [`Value`] to [`Unknown`] for [`crate::schema::parse::Schema::decode_unknown`].
pub fn unknown_from_serde_json(value: Value) -> Unknown {
  match value {
    Value::Null => Unknown::Null,
    Value::Bool(b) => Unknown::Bool(b),
    Value::Number(n) => {
      if let Some(i) = n.as_i64() {
        Unknown::I64(i)
      } else if let Some(u) = n.as_u64() {
        Unknown::I64(i64::try_from(u).unwrap_or(i64::MAX))
      } else if let Some(f) = n.as_f64() {
        Unknown::F64(f)
      } else {
        Unknown::String(n.to_string())
      }
    }
    Value::String(s) => Unknown::String(s),
    Value::Array(arr) => Unknown::Array(arr.into_iter().map(unknown_from_serde_json).collect()),
    Value::Object(map) => Unknown::Object(
      map
        .into_iter()
        .map(|(k, v)| (k, unknown_from_serde_json(v)))
        .collect::<BTreeMap<_, _>>(),
    ),
  }
}

/// Map [`Unknown`] back to JSON [`Value`] (inverse of [`unknown_from_serde_json`]).
///
/// Non-finite [`Unknown::F64`] values (NaN, ±∞) have no JSON form and map to [`Value::Null`].
pub fn serde_json_from_unknown(value: &Unknown) -> Value {
  match value {
    Unknown::Null => Value::Null,
    Unknown::Bool(b) => Value::Bool(*b),
    Unknown::I64(n) => Value::from(*n),
    Unknown::F64(x) => serde_json::Number::from_f64(*x).map_or(Value::Null, Value::Number),
    Unknown::String(s) => Value::String(s.clone()),
    Unknown::Array(items) => Value::Array(items.iter().map(serde_json_from_unknown).collect()),
    Unknown::Object(map) => Value::Object(
      map
        .iter()
        .map(|(k, v)| (k.clone(), serde_json_from_unknown(v)))
        .collect(),
    ),
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use serde_json::json;

  #[test]
  fn unknown_to_json_round_trips() {
    let v = json!({"a": [1, 2.5, "x", null, true], "b": {}});
    assert_eq!(
      serde_json_from_unknown(&unknown_from_serde_json(v.clone())),
      v
    );
    assert_eq!(
      serde_json_from_unknown(&Unknown::F64(f64::NAN)),
      Value::Null
    );
  }

  #[test]
  fn maps_float_to_f64_unknown() {
    let v = serde_json::json!(1.5);
    assert_eq!(unknown_from_serde_json(v), Unknown::F64(1.5));
  }

  #[test]
  fn maps_json_scalars_and_structure() {
    assert_eq!(unknown_from_serde_json(Value::Null), Unknown::Null);
    assert_eq!(unknown_from_serde_json(json!(true)), Unknown::Bool(true));
    assert_eq!(unknown_from_serde_json(json!(42)), Unknown::I64(42));
    assert_eq!(
      unknown_from_serde_json(json!(u64::MAX)),
      Unknown::I64(i64::MAX)
    );
    assert_eq!(
      unknown_from_serde_json(json!({"a": 1, "b": [null]})),
      Unknown::Object(
        [
          ("a".into(), Unknown::I64(1)),
          ("b".into(), Unknown::Array(vec![Unknown::Null])),
        ]
        .into_iter()
        .collect(),
      )
    );
  }
}
