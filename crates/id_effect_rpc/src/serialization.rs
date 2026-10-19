//! [`id_effect::schema`] bridging for tuple Schemas: decode a JSON payload, encode a tuple as a
//! named JSON object ([`StructWireJson`]). The wire messages themselves are [`crate::wire`].

use id_effect::schema::Schema;
use id_effect::schema::data::EffectData;
use id_effect::schema::serde_bridge::unknown_from_serde_json;
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

/// Serialization or schema validation failure at the RPC boundary.
#[derive(Debug, Error, PartialEq)]
pub enum RpcSerializationError {
  /// JSON parse or encode error.
  #[error("json: {0}")]
  Json(String),
  /// Schema decode/encode failure.
  #[error("schema at {path}: {message}")]
  Schema {
    /// Invalid field path.
    path: String,
    /// Validation message.
    message: String,
  },
  /// Invalid RPC protocol message.
  #[error("protocol: {0}")]
  Protocol(String),
}

impl From<serde_json::Error> for RpcSerializationError {
  fn from(err: serde_json::Error) -> Self {
    Self::Json(err.to_string())
  }
}

/// Decode `payload` with `schema` (first issue only).
pub fn decode_payload<A, I, E>(
  schema: &Schema<A, I, E>,
  payload: &Value,
) -> Result<A, RpcSerializationError>
where
  E: EffectData + 'static,
  A: 'static,
  I: 'static,
{
  let unknown = unknown_from_serde_json(payload.clone());
  schema
    .decode_unknown(&unknown)
    .map_err(|p| RpcSerializationError::Schema {
      path: p.path.clone(),
      message: p.message.clone(),
    })
}

/// Encode `value` with `schema` as an object named by `field_names`.
pub fn encode_struct_fields<A, I, E>(
  schema: &Schema<A, I, E>,
  field_names: &[&str],
  value: A,
) -> Result<Value, RpcSerializationError>
where
  E: EffectData + 'static,
  A: 'static,
  I: 'static,
  I: StructWireJson,
{
  schema
    .encode(value)
    .to_json_object(field_names)
    .map_err(RpcSerializationError::Json)
}

fn json_object(fields: &[(&str, serde_json::Value)]) -> Value {
  let mut map = serde_json::Map::new();
  for (k, v) in fields {
    map.insert((*k).to_string(), v.clone());
  }
  Value::Object(map)
}

/// Encode tuple wire values as JSON objects with field names.
pub trait StructWireJson {
  /// Map encoded wire tuple to JSON object using parallel field names.
  fn to_json_object(self, field_names: &[&str]) -> Result<Value, String>;
}

impl StructWireJson for () {
  fn to_json_object(self, field_names: &[&str]) -> Result<Value, String> {
    if field_names.is_empty() {
      Ok(Value::Object(Default::default()))
    } else {
      Err("unit wire cannot map to named fields".to_owned())
    }
  }
}

impl StructWireJson for String {
  fn to_json_object(self, field_names: &[&str]) -> Result<Value, String> {
    match field_names {
      [one] => {
        let mut obj = serde_json::Map::new();
        obj.insert(one.to_string(), Value::String(self));
        Ok(Value::Object(obj))
      }
      _ => Err(format!(
        "expected one field name for String wire, got {}",
        field_names.len()
      )),
    }
  }
}

impl<A0, A1> StructWireJson for (A0, A1)
where
  A0: Serialize,
  A1: Serialize,
{
  fn to_json_object(self, field_names: &[&str]) -> Result<Value, String> {
    let [n0, n1] = field_names else {
      return Err(format!(
        "expected two field names for pair wire, got {}",
        field_names.len()
      ));
    };
    Ok(json_object(&[
      (n0, serde_json::to_value(self.0).map_err(|e| e.to_string())?),
      (n1, serde_json::to_value(self.1).map_err(|e| e.to_string())?),
    ]))
  }
}

impl<A0, A1, A2> StructWireJson for (A0, A1, A2)
where
  A0: Serialize,
  A1: Serialize,
  A2: Serialize,
{
  fn to_json_object(self, field_names: &[&str]) -> Result<Value, String> {
    let [n0, n1, n2] = field_names else {
      return Err(format!(
        "expected three field names for triple wire, got {}",
        field_names.len()
      ));
    };
    Ok(json_object(&[
      (n0, serde_json::to_value(self.0).map_err(|e| e.to_string())?),
      (n1, serde_json::to_value(self.1).map_err(|e| e.to_string())?),
      (n2, serde_json::to_value(self.2).map_err(|e| e.to_string())?),
    ]))
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect::schema;

  #[test]
  fn decode_payload_validates_struct_fields() {
    let s = schema::struct_(
      "name",
      schema::string::<()>(),
      "enthusiasm",
      schema::i64::<()>(),
    );
    let payload = serde_json::json!({"name": "Ada", "enthusiasm": 2});
    let (name, n) = decode_payload(&s, &payload).expect("ok");
    assert_eq!(name, "Ada");
    assert_eq!(n, 2);
    let err = decode_payload(&s, &serde_json::json!({"name": 1})).unwrap_err();
    assert!(matches!(err, RpcSerializationError::Schema { .. }));
  }

  #[test]
  fn errors_display() {
    let cases = [
      RpcSerializationError::Json("j".into()),
      RpcSerializationError::Schema {
        path: "a".into(),
        message: "m".into(),
      },
      RpcSerializationError::Protocol("p".into()),
    ];
    for e in cases {
      assert!(!e.to_string().is_empty());
    }
    let from: RpcSerializationError = serde_json::from_str::<u8>("x").unwrap_err().into();
    assert!(matches!(from, RpcSerializationError::Json(_)));
  }

  #[test]
  fn struct_wire_json_maps_tuples_to_named_fields() {
    assert_eq!(().to_json_object(&[]).unwrap(), serde_json::json!({}));
    assert!(().to_json_object(&["x"]).is_err());
    assert_eq!(
      "v".to_owned().to_json_object(&["x"]).unwrap(),
      serde_json::json!({"x": "v"})
    );
    assert!("v".to_owned().to_json_object(&[]).is_err());
    assert!((1, 2).to_json_object(&["a"]).is_err());
    assert_eq!(
      (1, "b", true).to_json_object(&["a", "b", "c"]).unwrap(),
      serde_json::json!({"a": 1, "b": "b", "c": true})
    );
    assert!((1, 2, 3).to_json_object(&["a", "b"]).is_err());
  }

  #[test]
  fn encode_struct_fields_round_trip() {
    let s = schema::struct_(
      "message",
      schema::string::<()>(),
      "count",
      schema::i64::<()>(),
    );
    let json =
      encode_struct_fields(&s, &["message", "count"], ("hi".to_owned(), 3)).expect("encode");
    assert_eq!(json, serde_json::json!({"message": "hi", "count": 3}));
    assert!(encode_struct_fields(&s, &["message"], ("hi".to_owned(), 3)).is_err());
  }
}
