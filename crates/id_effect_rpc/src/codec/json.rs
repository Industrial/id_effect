//! The JSON codec (`application/json`, `idrpc.json.v1`), the default.

use std::fmt;

use serde::de::{self, DeserializeOwned, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Number, Value};

use super::{Codec, CodecError, CodecId};

/// The encode buffer's starting size (bytes).
const INITIAL_CAPACITY: usize = 256;

/// JSON through `serde_json`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Json;

impl Codec for Json {
  const ID: CodecId = CodecId::Json;

  fn encode<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>, CodecError> {
    // A typical result fits: no reallocation while the writer grows the buffer.
    let mut out = Vec::with_capacity(INITIAL_CAPACITY);
    serde_json::to_writer(&mut out, value).map_err(|e| CodecError::Encode(e.to_string()))?;
    Ok(out)
  }

  fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, CodecError> {
    serde_json::from_slice(bytes).map_err(|e| CodecError::Decode(e.to_string()))
  }
}

/// The object key serde_json's `raw_value` feature (on in the workspace through axum and sqlx)
/// reads, as the first key of an object becoming a [`Value`], as "this string is raw JSON text".
///
/// Not first, it is an ordinary member; re-encoding sorts the keys, so it comes first and the next
/// decode replaces the object with the JSON its string holds (or fails). Untrusted JSON carrying it
/// is refused: see [`StrictValue`].
pub(crate) const RAW_VALUE_KEY: &str = "$serde_json::private::RawValue";

/// A [`Value`] decoded refusing any object with the key [`RAW_VALUE_KEY`], at any depth and in any
/// position, so decode, encode, decode gives the same value. Every JSON payload from the network
/// decodes through it (frames, batches, call inputs, NDJSON items, postcard failures).
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct StrictValue(pub(crate) Value);

impl<'de> Deserialize<'de> for StrictValue {
  fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
    deserializer.deserialize_any(StrictVisitor).map(Self)
  }
}

/// Decode untrusted JSON `bytes` as a [`StrictValue`].
pub(crate) fn decode_value(bytes: &[u8]) -> Result<Value, CodecError> {
  Json::decode::<StrictValue>(bytes).map(|strict| strict.0)
}

/// Builds a [`Value`] like serde_json's own visitor, minus the `raw_value` key classification.
struct StrictVisitor;

impl<'de> Visitor<'de> for StrictVisitor {
  type Value = Value;

  fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("any JSON value")
  }

  fn visit_bool<E>(self, v: bool) -> Result<Value, E> {
    Ok(Value::Bool(v))
  }

  fn visit_i64<E>(self, v: i64) -> Result<Value, E> {
    Ok(Value::from(v))
  }

  fn visit_u64<E>(self, v: u64) -> Result<Value, E> {
    Ok(Value::from(v))
  }

  fn visit_f64<E>(self, v: f64) -> Result<Value, E> {
    Ok(Number::from_f64(v).map_or(Value::Null, Value::Number))
  }

  fn visit_str<E>(self, v: &str) -> Result<Value, E> {
    Ok(Value::String(v.to_owned()))
  }

  fn visit_string<E>(self, v: String) -> Result<Value, E> {
    Ok(Value::String(v))
  }

  fn visit_unit<E>(self) -> Result<Value, E> {
    Ok(Value::Null)
  }

  fn visit_none<E>(self) -> Result<Value, E> {
    Ok(Value::Null)
  }

  fn visit_some<D: Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
    StrictValue::deserialize(deserializer).map(|strict| strict.0)
  }

  fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
    let mut items = Vec::new();
    while let Some(StrictValue(item)) = seq.next_element()? {
      items.push(item);
    }
    Ok(Value::Array(items))
  }

  fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
    let mut members = Map::new();
    while let Some(key) = map.next_key::<String>()? {
      if key == RAW_VALUE_KEY {
        return Err(de::Error::custom(format_args!(
          "the object key `{RAW_VALUE_KEY}` is reserved"
        )));
      }
      let StrictValue(value) = map.next_value()?;
      members.insert(key, value);
    }
    Ok(Value::Object(members))
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::collections::BTreeMap;

  struct Unencodable;

  impl Serialize for Unencodable {
    fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
      Err(serde::ser::Error::custom("no"))
    }
  }

  #[test]
  fn round_trips_and_reports_errors() {
    let mut map = BTreeMap::new();
    map.insert("k", 1);
    let bytes = Json::encode(&map).unwrap();
    assert_eq!(bytes, br#"{"k":1}"#);
    let back: BTreeMap<String, u8> = Json::decode(&bytes).unwrap();
    assert_eq!(back["k"], 1);
    assert!(matches!(
      Json::encode(&Unencodable),
      Err(CodecError::Encode(_))
    ));
    assert!(matches!(
      Json::decode::<u8>(b"[]"),
      Err(CodecError::Decode(_))
    ));
  }
}
