//! The JSON codec (`application/json`, `idrpc.json.v1`), the default.

use serde::Serialize;
use serde::de::DeserializeOwned;

use super::{Codec, CodecError, CodecId};

/// JSON through `serde_json`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Json;

impl Codec for Json {
  const ID: CodecId = CodecId::Json;

  fn encode<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>, CodecError> {
    serde_json::to_vec(value).map_err(|e| CodecError::Encode(e.to_string()))
  }

  fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, CodecError> {
    serde_json::from_slice(bytes).map_err(|e| CodecError::Decode(e.to_string()))
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
