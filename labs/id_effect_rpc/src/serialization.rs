//! JSON serialization and [`id_effect::schema`] bridging for RPC wire messages.

use id_effect::data::EffectData;
use id_effect::schema::Schema;
use id_effect::schema::serde_bridge::unknown_from_serde_json;
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

use crate::RpcError;
use crate::protocol::{RpcWireRequest, RpcWireResponse};

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

/// RPC serialization helper.
pub fn decode_wire_request(bytes: &[u8]) -> Result<RpcWireRequest, RpcSerializationError> {
  Ok(serde_json::from_slice(bytes)?)
}

/// RPC serialization helper.
pub fn encode_wire_response(response: &RpcWireResponse) -> Result<Vec<u8>, RpcSerializationError> {
  Ok(serde_json::to_vec(response)?)
}

/// RPC serialization helper.
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

/// RPC serialization helper.
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

/// RPC serialization helper.
pub fn validate_success_json<A, I, E>(
  schema: &Schema<A, I, E>,
  success: &Value,
) -> Result<A, RpcSerializationError>
where
  E: EffectData + 'static,
  A: 'static,
  I: 'static,
{
  decode_payload(schema, success)
}

/// RPC serialization helper.
pub fn serialization_to_rpc_error(err: RpcSerializationError) -> RpcError {
  match err {
    RpcSerializationError::Json(msg) => RpcError::invalid_argument(msg),
    RpcSerializationError::Schema { path, message } => {
      RpcError::invalid_argument(format!("{path}: {message}"))
    }
    RpcSerializationError::Protocol(msg) => RpcError::invalid_argument(msg),
  }
}

/// RPC serialization helper.
pub fn rpc_error_to_wire(tag: &str, err: RpcError) -> RpcWireResponse {
  RpcWireResponse::failure(tag, err.envelope().clone())
}

/// RPC serialization helper.
pub fn success_wire<A, I, E>(
  tag: &str,
  schema: &Schema<A, I, E>,
  field_names: &[&str],
  value: A,
) -> Result<RpcWireResponse, RpcSerializationError>
where
  E: EffectData + 'static,
  A: 'static,
  I: 'static,
  I: StructWireJson,
{
  let success = encode_struct_fields(schema, field_names, value)?;
  Ok(RpcWireResponse::success(tag, success))
}

/// RPC serialization helper.
pub fn success_json(tag: &str, value: Value) -> RpcWireResponse {
  RpcWireResponse::success(tag, value)
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
  }
}
