//! Codecs and their negotiation (ADR 0014 D13, design note §7).
//!
//! A [`Codec`] turns wire types into bytes and back; [`CodecId`] is the codec a connection or a
//! request negotiated: from `Content-Type` ([`negotiate_content_type`]) on HTTP, from the
//! offered subprotocols ([`negotiate_subprotocol`]) on WebSocket. JSON is the default; an
//! unknown codec is refused (HTTP 415), never silently replaced by JSON.
//!
//! ```rust
//! use id_effect_rpc::codec::{Codec, CodecId, Json, negotiate_content_type, negotiate_subprotocol};
//!
//! assert_eq!(negotiate_content_type(None).unwrap(), CodecId::Json);
//! assert_eq!(negotiate_content_type(Some("application/json; charset=utf-8")).unwrap(), CodecId::Json);
//! assert!(negotiate_content_type(Some("application/x-www-form-urlencoded")).is_err());
//! assert_eq!(negotiate_subprotocol(["idrpc.cbor.v9", "idrpc.json.v1"]), Some(CodecId::Json));
//!
//! let bytes = Json::encode(&vec![1, 2]).unwrap();
//! assert_eq!(Json::decode::<Vec<u8>>(&bytes).unwrap(), [1, 2]);
//! ```

mod json;

pub use json::Json;

use serde::Serialize;
use serde::de::DeserializeOwned;

/// Media type of NDJSON stream bodies (server streams, client streams).
pub const NDJSON: &str = "application/x-ndjson";

/// A wire encoding.
pub trait Codec: Send + Sync + 'static {
  /// Which codec this is.
  const ID: CodecId;

  /// Encode `value`.
  fn encode<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>, CodecError>;

  /// Decode a `T` from `bytes` (Schema validation runs after, at the boundary).
  fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, CodecError>;
}

/// The negotiated codec of a request or connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CodecId {
  /// JSON (the default).
  Json,
}

impl CodecId {
  /// Every codec this build knows, in server preference order.
  pub const ALL: &'static [Self] = &[Self::Json];

  /// Short name (`json`).
  pub fn name(self) -> &'static str {
    match self {
      Self::Json => "json",
    }
  }

  /// HTTP media type (`application/json`).
  pub fn content_type(self) -> &'static str {
    match self {
      Self::Json => "application/json",
    }
  }

  /// WebSocket subprotocol (`idrpc.json.v1`).
  pub fn subprotocol(self) -> &'static str {
    match self {
      Self::Json => "idrpc.json.v1",
    }
  }

  /// Encode `value` with this codec.
  pub fn encode<T: Serialize + ?Sized>(self, value: &T) -> Result<Vec<u8>, CodecError> {
    match self {
      Self::Json => Json::encode(value),
    }
  }

  /// Decode a `T` with this codec.
  pub fn decode<T: DeserializeOwned>(self, bytes: &[u8]) -> Result<T, CodecError> {
    match self {
      Self::Json => Json::decode(bytes),
    }
  }
}

/// A codec failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CodecError {
  /// No codec of this build matches the media type or subprotocol (HTTP 415).
  #[error("unsupported codec `{0}`")]
  Unsupported(String),
  /// The value could not be encoded.
  #[error("encode: {0}")]
  Encode(String),
  /// The bytes are not a value of the expected type.
  #[error("decode: {0}")]
  Decode(String),
}

/// The codec of a request body from its `Content-Type`: absent means JSON; media type
/// parameters (`charset`) are ignored and the type compares case-insensitively.
pub fn negotiate_content_type(content_type: Option<&str>) -> Result<CodecId, CodecError> {
  let Some(raw) = content_type else {
    return Ok(CodecId::Json);
  };
  let essence = raw.split(';').next().unwrap_or_default().trim();
  CodecId::ALL
    .iter()
    .copied()
    .find(|codec| codec.content_type().eq_ignore_ascii_case(essence))
    .ok_or_else(|| CodecError::Unsupported(essence.to_owned()))
}

/// The first offered WebSocket subprotocol this build knows (`None`: refuse the upgrade).
pub fn negotiate_subprotocol<'a>(offered: impl IntoIterator<Item = &'a str>) -> Option<CodecId> {
  offered.into_iter().find_map(|name| {
    let name = name.trim();
    CodecId::ALL
      .iter()
      .copied()
      .find(|codec| codec.subprotocol() == name)
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn json_is_the_default_and_named() {
    let json = CodecId::Json;
    assert_eq!(json.name(), "json");
    assert_eq!(json.content_type(), "application/json");
    assert_eq!(json.subprotocol(), "idrpc.json.v1");
    assert_eq!(Json::ID, json);
    assert_eq!(negotiate_content_type(None), Ok(json));
    assert_eq!(negotiate_content_type(Some("Application/JSON")), Ok(json));
    assert_eq!(NDJSON, "application/x-ndjson");
  }

  #[test]
  fn unknown_codecs_are_refused() {
    let err = negotiate_content_type(Some("text/plain; charset=utf-8")).unwrap_err();
    assert_eq!(err, CodecError::Unsupported("text/plain".into()));
    assert!(err.to_string().contains("unsupported codec"));
    assert_eq!(negotiate_subprotocol(["chat", "idrpc.json.v2"]), None);
    assert_eq!(
      negotiate_subprotocol([" idrpc.json.v1 "]),
      Some(CodecId::Json)
    );
  }

  #[test]
  fn codec_ids_encode_and_decode() {
    let bytes = CodecId::Json.encode(&("a", 1)).unwrap();
    assert_eq!(bytes, br#"["a",1]"#);
    let back: (String, u8) = CodecId::Json.decode(&bytes).unwrap();
    assert_eq!(back, ("a".to_owned(), 1));
    let err = CodecId::Json.decode::<u8>(b"nope").unwrap_err();
    assert!(matches!(err, CodecError::Decode(_)));
  }
}
