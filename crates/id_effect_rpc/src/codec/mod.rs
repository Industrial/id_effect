//! Codecs and their negotiation (ADR 0014 D13, design note §7).
//!
//! A [`Codec`] turns wire types into bytes and back; [`CodecId`] is the codec a connection or a
//! request negotiated: from `Content-Type` ([`negotiate_content_type`]) and `Accept`
//! ([`negotiate_accept`]) on HTTP, from the offered subprotocols ([`negotiate_subprotocol`]) on
//! WebSocket. JSON is the default; an unknown codec is refused (HTTP 415), never silently
//! replaced by JSON.
//!
//! | Codec | HTTP media type | WebSocket subprotocol |
//! |---|---|---|
//! | JSON (default) | `application/json` | `idrpc.json.v1` |
//! | postcard (feature `postcard`) | `application/vnd.id-effect.rpc+postcard` | `idrpc.postcard.v1` |
//!
//! JSON is self-describing, so the erased handlers (which carry `serde_json::Value`) take it as
//! it comes. postcard is not: a procedure answers postcard only when its schemas carry a
//! [`TypedCodec`] ([`WireSchema::with_serde`]); the registry keeps both as the entry's
//! [`ProcedureCodecs`]. The typed bridge decodes `T` with serde, then hands the handler the
//! value the Schema encodes, so the Schema's validators (refinements, brands) still run on it.
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
#[cfg(feature = "postcard")]
pub mod postcard;

#[cfg(feature = "postcard")]
pub use self::postcard::Postcard;
pub use json::Json;

use std::sync::Arc;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::contract::ProcedureKind;
use crate::handler::WireSchema;
use crate::wire::WireExit;

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
  /// postcard (feature `postcard`): compact, not self-describing; typed procedures only.
  #[cfg(feature = "postcard")]
  Postcard,
}

impl CodecId {
  /// Every codec this build knows, in server preference order.
  #[cfg(not(feature = "postcard"))]
  pub const ALL: &'static [Self] = &[Self::Json];

  /// Every codec this build knows, in server preference order.
  #[cfg(feature = "postcard")]
  pub const ALL: &'static [Self] = &[Self::Json, Self::Postcard];

  /// Short name (`json`, `postcard`).
  pub fn name(self) -> &'static str {
    match self {
      Self::Json => "json",
      #[cfg(feature = "postcard")]
      Self::Postcard => "postcard",
    }
  }

  /// HTTP media type (`application/json`, `application/vnd.id-effect.rpc+postcard`).
  pub fn content_type(self) -> &'static str {
    match self {
      Self::Json => "application/json",
      #[cfg(feature = "postcard")]
      Self::Postcard => self::postcard::MEDIA_TYPE,
    }
  }

  /// WebSocket subprotocol (`idrpc.json.v1`, `idrpc.postcard.v1`).
  pub fn subprotocol(self) -> &'static str {
    match self {
      Self::Json => "idrpc.json.v1",
      #[cfg(feature = "postcard")]
      Self::Postcard => self::postcard::SUBPROTOCOL,
    }
  }

  /// Whether a value decodes without knowing its type (JSON yes, postcard no).
  pub fn is_self_describing(self) -> bool {
    match self {
      Self::Json => true,
      #[cfg(feature = "postcard")]
      Self::Postcard => false,
    }
  }

  /// Encode `value` with this codec.
  pub fn encode<T: Serialize + ?Sized>(self, value: &T) -> Result<Vec<u8>, CodecError> {
    match self {
      Self::Json => Json::encode(value),
      #[cfg(feature = "postcard")]
      Self::Postcard => Postcard::encode(value),
    }
  }

  /// Decode a `T` with this codec (postcard: at most
  /// [`MAX_FRAME_BYTES`](crate::wire::MAX_FRAME_BYTES), nesting and lengths bounded).
  pub fn decode<T: DeserializeOwned>(self, bytes: &[u8]) -> Result<T, CodecError> {
    match self {
      Self::Json => Json::decode(bytes),
      #[cfg(feature = "postcard")]
      Self::Postcard => Postcard::decode(bytes),
    }
  }
}

/// A codec failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CodecError {
  /// No codec of this build matches the media type or subprotocol, or the procedure does not
  /// take this codec (HTTP 415).
  #[error("unsupported codec `{0}`")]
  Unsupported(String),
  /// The value could not be encoded.
  #[error("encode: {0}")]
  Encode(String),
  /// The bytes are not a value of the expected type.
  #[error("decode: {0}")]
  Decode(String),
  /// The input is larger than the decoder accepts; refused before decoding (HTTP 413,
  /// WebSocket close 1009).
  #[error("{size} bytes exceed the limit of {limit}")]
  TooLarge {
    /// The input's size.
    size: usize,
    /// The limit.
    limit: usize,
  },
}

fn essence(media_type: &str) -> &str {
  media_type.split(';').next().unwrap_or_default().trim()
}

fn by_media_type(essence: &str) -> Option<CodecId> {
  CodecId::ALL
    .iter()
    .copied()
    .find(|codec| codec.content_type().eq_ignore_ascii_case(essence))
}

/// The codec of a request body from its `Content-Type`: absent means JSON; media type
/// parameters (`charset`) are ignored and the type compares case-insensitively.
pub fn negotiate_content_type(content_type: Option<&str>) -> Result<CodecId, CodecError> {
  let Some(raw) = content_type else {
    return Ok(CodecId::Json);
  };
  let essence = essence(raw);
  by_media_type(essence).ok_or_else(|| CodecError::Unsupported(essence.to_owned()))
}

/// The codec of a response from the request's `Accept` and the request's codec: absent, empty,
/// `*/*` or `application/*` mean the request codec; otherwise the first listed media type this
/// build knows (ranges with `q=0` are skipped). Nothing known is
/// [`CodecError::Unsupported`] (HTTP 415, design note §7) — never a silent JSON answer.
pub fn negotiate_accept(accept: Option<&str>, request: CodecId) -> Result<CodecId, CodecError> {
  let Some(raw) = accept.filter(|a| !a.trim().is_empty()) else {
    return Ok(request);
  };
  let refused = |range: &str| {
    range.split(';').skip(1).any(|param| {
      let mut kv = param.splitn(2, '=');
      let key = kv.next().unwrap_or_default().trim();
      let value = kv.next().unwrap_or_default().trim();
      key.eq_ignore_ascii_case("q") && value.parse::<f32>().is_ok_and(|q| q <= 0.0)
    })
  };
  for range in raw.split(',').filter(|range| !refused(range)) {
    let essence = essence(range);
    if essence == "*/*" || essence.eq_ignore_ascii_case("application/*") {
      return Ok(request);
    }
    if let Some(codec) = by_media_type(essence) {
      return Ok(codec);
    }
  }
  Err(CodecError::Unsupported(raw.trim().to_owned()))
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

// ---------------------------------------------------------------------------------------------
// The typed bridge
// ---------------------------------------------------------------------------------------------

type TypedDecodeFn = Arc<dyn Fn(CodecId, &[u8]) -> Result<Value, CodecError> + Send + Sync>;
type TypedEncodeFn = Arc<dyn Fn(CodecId, &Value) -> Result<Vec<u8>, CodecError> + Send + Sync>;

/// One wire type through serde, for codecs that are not self-describing (postcard).
///
/// Decoding reads a `T` with the codec, then encodes it with the type's [`WireSchema`]: the
/// handler receives the same JSON value a JSON client would have sent, and the Schema decode the
/// erased handler runs next applies every validator (a refinement or a brand that a serde
/// `Deserialize` does not check is still refused, as `InvalidInput`). Encoding decodes the
/// handler's JSON value with the Schema and writes the `T` with the codec.
#[derive(Clone)]
pub struct TypedCodec {
  decode: TypedDecodeFn,
  encode: TypedEncodeFn,
}

impl std::fmt::Debug for TypedCodec {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str("TypedCodec(..)")
  }
}

impl TypedCodec {
  /// The bridge of `schema`'s type `T`.
  pub fn of<T>(schema: &WireSchema<T>) -> Self
  where
    T: Serialize + DeserializeOwned + 'static,
  {
    let decoder = schema.clone();
    let encoder = schema.clone();
    Self {
      decode: Arc::new(move |codec, bytes| {
        let value: T = codec.decode(bytes)?;
        decoder.encode(value).map_err(CodecError::Decode)
      }),
      encode: Arc::new(move |codec, value| {
        let typed = encoder.decode(value).map_err(|issues| {
          let paths: Vec<_> = issues.iter().map(|i| i.path.as_str()).collect();
          CodecError::Encode(format!(
            "the value does not decode with its Schema at {paths:?}"
          ))
        })?;
        codec.encode(&typed)
      }),
    }
  }

  /// Decode `bytes` as `T` with `codec`: the value the handler receives.
  pub fn decode(&self, codec: CodecId, bytes: &[u8]) -> Result<Value, CodecError> {
    (self.decode)(codec, bytes)
  }

  /// Encode the handler's `value` as `T` with `codec`.
  pub fn encode(&self, codec: CodecId, value: &Value) -> Result<Vec<u8>, CodecError> {
    (self.encode)(codec, value)
  }
}

/// The typed bridges of one registered procedure (`RpcMethodEntry::codecs`).
///
/// `input` is what the client sends: the call input (query, mutation, server stream,
/// subscription) or one item (client stream, bidi). `output` is what the server sends: the
/// output (query, mutation, client stream) or one item (server stream, bidi, subscription).
/// The exit value of the item-producing kinds is `null` and needs no bridge.
#[derive(Debug, Clone, Default)]
pub struct ProcedureCodecs {
  input: Option<TypedCodec>,
  output: Option<TypedCodec>,
  unit_exit: bool,
}

impl ProcedureCodecs {
  /// The bridges of a procedure of `kind`.
  pub fn new(kind: ProcedureKind, input: Option<TypedCodec>, output: Option<TypedCodec>) -> Self {
    Self {
      input,
      output,
      unit_exit: matches!(
        kind,
        ProcedureKind::ServerStream | ProcedureKind::Bidi | ProcedureKind::Subscription
      ),
    }
  }

  /// Whether both directions have a bridge (the procedure takes every codec of this build).
  pub fn is_typed(&self) -> bool {
    self.input.is_some() && self.output.is_some()
  }

  /// Whether the procedure takes `codec` in both directions.
  pub fn supports(&self, codec: CodecId) -> bool {
    codec.is_self_describing() || self.is_typed()
  }

  /// The input bridge.
  pub fn input(&self) -> Option<&TypedCodec> {
    self.input.as_ref()
  }

  /// The output bridge.
  pub fn output(&self) -> Option<&TypedCodec> {
    self.output.as_ref()
  }

  /// Decode an input (or client item) sent with `codec` into the handler's value.
  ///
  /// A self-describing codec (JSON) is read as the Schema's JSON form and needs no bridge;
  /// another codec goes through the input bridge, and without one is
  /// [`CodecError::Unsupported`] (415).
  pub fn decode_input(&self, codec: CodecId, bytes: &[u8]) -> Result<Value, CodecError> {
    match &self.input {
      _ if codec.is_self_describing() => codec.decode(bytes),
      Some(typed) => typed.decode(codec, bytes),
      None => Err(unsupported(codec)),
    }
  }

  /// Encode an output (or server item) for `codec`.
  pub fn encode_output(&self, codec: CodecId, value: &Value) -> Result<Vec<u8>, CodecError> {
    match &self.output {
      _ if codec.is_self_describing() => codec.encode(value),
      Some(typed) => typed.encode(codec, value),
      None => Err(unsupported(codec)),
    }
  }

  /// A call's exit with its success value encoded for `codec` (empty for the `null` exit of a
  /// stream kind); a failure keeps its error union value.
  pub fn encode_exit(
    &self,
    codec: CodecId,
    exit: &WireExit,
  ) -> Result<WireExit<Vec<u8>, Value>, CodecError> {
    Ok(match exit {
      WireExit::Success { .. } if self.unit_exit => WireExit::Success { value: Vec::new() },
      WireExit::Success { value } => WireExit::Success {
        value: self.encode_output(codec, value)?,
      },
      WireExit::Failure { error } => WireExit::Failure {
        error: error.clone(),
      },
    })
  }
}

fn unsupported(codec: CodecId) -> CodecError {
  CodecError::Unsupported(format!(
    "{} (this procedure's schemas carry no typed codec: `WireSchema::with_serde`)",
    codec.content_type()
  ))
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect::schema;

  #[test]
  fn json_is_the_default_and_named() {
    let json = CodecId::Json;
    assert_eq!(json.name(), "json");
    assert_eq!(json.content_type(), "application/json");
    assert_eq!(json.subprotocol(), "idrpc.json.v1");
    assert!(json.is_self_describing());
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
    let big = CodecError::TooLarge { size: 9, limit: 8 };
    assert_eq!(big.to_string(), "9 bytes exceed the limit of 8");
  }

  #[test]
  fn accept_picks_the_response_codec() {
    let json = CodecId::Json;
    assert_eq!(negotiate_accept(None, json), Ok(json));
    assert_eq!(negotiate_accept(Some(" "), json), Ok(json));
    assert_eq!(negotiate_accept(Some("*/*"), json), Ok(json));
    assert_eq!(
      negotiate_accept(Some("Application/*;q=0.5"), json),
      Ok(json)
    );
    assert_eq!(
      negotiate_accept(Some("text/html, application/json;q=0.9"), json),
      Ok(json)
    );
    assert_eq!(
      negotiate_accept(Some("text/html"), json),
      Err(CodecError::Unsupported("text/html".into()))
    );
    assert!(negotiate_accept(Some("application/json;q=0"), json).is_err());
    assert!(negotiate_accept(Some("application/json;q=zero, */*;q=0.0"), json).is_ok());
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

  #[test]
  fn json_needs_no_bridge_and_typed_bridges_validate() {
    let untyped = ProcedureCodecs::default();
    assert!(!untyped.is_typed());
    assert!(untyped.supports(CodecId::Json));
    assert!(untyped.input().is_none() && untyped.output().is_none());
    assert_eq!(
      untyped.decode_input(CodecId::Json, b"7").unwrap(),
      Value::from(7)
    );
    assert_eq!(
      untyped
        .encode_output(CodecId::Json, &Value::from("x"))
        .unwrap(),
      br#""x""#
    );

    let short = WireSchema::new(schema::max_length(schema::string::<()>(), 3)).with_serde();
    let typed = ProcedureCodecs::new(
      ProcedureKind::Query,
      short.typed_codec().cloned(),
      short.typed_codec().cloned(),
    );
    assert!(typed.is_typed());
    let bridge = typed.input().unwrap();
    assert!(format!("{bridge:?}").contains("TypedCodec"));
    assert_eq!(
      bridge.decode(CodecId::Json, br#""abcd""#).unwrap(),
      Value::from("abcd"),
      "decoding does not validate: the handler's Schema decode does"
    );
    let err = bridge
      .encode(CodecId::Json, &Value::from("abcd"))
      .unwrap_err();
    assert!(matches!(err, CodecError::Encode(_)), "{err}");
    assert!(matches!(
      bridge.decode(CodecId::Json, b"1"),
      Err(CodecError::Decode(_))
    ));
    let unencodable =
      WireSchema::<i64>::from_fns(|_| Ok(1), |_| Err("no wire form".to_owned()), Value::Null)
        .with_serde();
    assert_eq!(
      unencodable
        .typed_codec()
        .unwrap()
        .decode(CodecId::Json, b"1"),
      Err(CodecError::Decode("no wire form".into()))
    );
    // JSON itself is never routed through the bridge: it is the Schema's own JSON form.
    assert_eq!(
      typed.decode_input(CodecId::Json, br#""abcd""#).unwrap(),
      Value::from("abcd")
    );
    assert_eq!(
      typed
        .encode_output(CodecId::Json, &Value::from("abcd"))
        .unwrap(),
      br#""abcd""#
    );
    let ok = typed
      .encode_exit(
        CodecId::Json,
        &WireExit::Success {
          value: Value::from("ab"),
        },
      )
      .unwrap();
    assert_eq!(
      ok,
      WireExit::Success {
        value: br#""ab""#.to_vec()
      }
    );
    let failure = WireExit::Failure {
      error: serde_json::json!({"_tag": "Cancelled"}),
    };
    assert_eq!(
      typed.encode_exit(CodecId::Json, &failure).unwrap(),
      WireExit::Failure {
        error: serde_json::json!({"_tag": "Cancelled"})
      }
    );
    let stream = ProcedureCodecs::new(ProcedureKind::ServerStream, None, None);
    assert_eq!(
      stream
        .encode_exit(CodecId::Json, &WireExit::Success { value: Value::Null })
        .unwrap(),
      WireExit::Success { value: Vec::new() }
    );
  }
}
