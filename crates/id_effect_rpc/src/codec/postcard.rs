//! The postcard codec (feature `postcard`; ADR 0014 D13, design note §5 and §7).
//!
//! | | |
//! |---|---|
//! | HTTP media type | `application/vnd.id-effect.rpc+postcard` ([`MEDIA_TYPE`]) |
//! | stream bodies | u32 big-endian length-prefixed records ([`STREAM_MEDIA_TYPE`], [`encode_record`], [`split_record`]) |
//! | WebSocket subprotocol | `idrpc.postcard.v1` ([`SUBPROTOCOL`]); one binary message per frame |
//! | mesh | always postcard |
//!
//! postcard is not self-describing, so values cross it typed: a procedure takes postcard when
//! its schemas carry a [`TypedCodec`](super::TypedCodec) (`WireSchema::with_serde`), and the
//! payload of a frame, call or exit is that type's postcard bytes, nested as a byte string. The
//! envelopes themselves are fixed here:
//!
//! - [`Frame`]: `id` (varint), `kind` (one byte, the [`FrameKind`] index of design note §5, never
//!   reordered: [`frame_kind_index`]), `flags` (one byte), then `tag`, `v`, `cursor`, `credits`,
//!   `payload`, each an option (`0` absent, `1` present).
//! - [`WireCall`]: `id?`, `tag`, `v?`, `payload`; [`WireResult`]: `id?`, `tag`, `exit`.
//! - [`WireExit`]: variant `0` `Success` with the output's bytes (empty for the `null` exit of a
//!   stream kind), variant `1` `Failure` with the error union as JSON text (error unions are
//!   tagged unions `{"_tag": …}` of the contract, not one Rust type).
//!
//! Every decoder here is bounded for hostile input: the input is refused before decoding when
//! it is over [`MAX_FRAME_BYTES`] (64 KiB, C3); nesting is limited to [`MAX_DEPTH`]; the elements
//! of all sequences and maps together are limited by the input's size (so a sequence of
//! zero-sized values cannot spin); length prefixes never preallocate more than the input holds;
//! strings and bytes are checked against the remaining input before they are copied; trailing
//! bytes are refused. [`decode_frame`] is the entry point of the `rpc_postcard_frame` fuzz
//! target.
//!
//! ```rust
//! use id_effect_rpc::codec::postcard::{Postcard, decode_frame, encode_frame};
//! use id_effect_rpc::codec::Codec;
//! use id_effect_rpc::wire::{CallId, Frame};
//!
//! let input = Postcard::encode(&("Ada".to_owned(), 36_i64)).unwrap();
//! let call = Frame::call(CallId::new(1).unwrap(), "Users.add", input.clone());
//! let bytes = encode_frame(&call).unwrap();
//! let back = decode_frame(&bytes).unwrap();
//! assert_eq!(back, call);
//! let (name, age): (String, i64) = Postcard::decode(back.payload.as_deref().unwrap()).unwrap();
//! assert_eq!((name.as_str(), age), ("Ada", 36));
//! ```

use std::cell::Cell;
use std::fmt;

use serde::de::{
  self, DeserializeOwned, DeserializeSeed, EnumAccess, MapAccess, SeqAccess, VariantAccess, Visitor,
};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use super::{Codec, CodecError, CodecId, ProcedureCodecs};
use crate::registry::is_valid_tag;
use crate::wire::{
  CallId, Cursor, Frame, FrameFlags, FrameKind, Hello, WireCall, WireExit, WireResult,
};

pub use crate::wire::MAX_FRAME_BYTES;

/// HTTP media type of a postcard body.
pub const MEDIA_TYPE: &str = "application/vnd.id-effect.rpc+postcard";

/// HTTP media type of a postcard stream body: u32 big-endian length-prefixed frames.
pub const STREAM_MEDIA_TYPE: &str = "application/vnd.id-effect.rpc+postcard; stream=1";

/// WebSocket subprotocol of postcard connections.
pub const SUBPROTOCOL: &str = "idrpc.postcard.v1";

/// Deepest nesting a decoder accepts (structs, sequences, maps, options, enums, newtypes).
pub const MAX_DEPTH: usize = 64;

/// Bytes of a record's length prefix.
pub const RECORD_HEADER_BYTES: usize = 4;

/// postcard through `serde`, every decode bounded ([`decode_limited`] at [`MAX_FRAME_BYTES`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Postcard;

impl Codec for Postcard {
  const ID: CodecId = CodecId::Postcard;

  fn encode<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>, CodecError> {
    ::postcard::to_stdvec(value).map_err(|e| CodecError::Encode(e.to_string()))
  }

  fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, CodecError> {
    decode_limited(bytes, MAX_FRAME_BYTES)
  }
}

// ---------------------------------------------------------------------------------------------
// Bounded decoding
// ---------------------------------------------------------------------------------------------

/// What one decode may still spend.
struct Budget {
  depth: Cell<usize>,
  elements: Cell<usize>,
  input: usize,
  why: Cell<Option<&'static str>>,
}

impl Budget {
  fn new(input: usize) -> Self {
    Self {
      depth: Cell::new(0),
      // Each element of a sequence costs at least one byte unless it is zero-sized; struct
      // fields and tuple members are counted too, hence the slack.
      elements: Cell::new(input.saturating_mul(8).saturating_add(1024)),
      input,
      why: Cell::new(None),
    }
  }

  fn fail<E: de::Error>(&self, why: &'static str) -> E {
    self.why.set(Some(why));
    E::custom(why)
  }

  fn enter<E: de::Error>(&self) -> Result<Depth<'_>, E> {
    let depth = self.depth.get() + 1;
    if depth > MAX_DEPTH {
      return Err(self.fail("nesting deeper than the limit"));
    }
    self.depth.set(depth);
    Ok(Depth(self))
  }

  fn element<E: de::Error>(&self) -> Result<(), E> {
    match self.elements.get().checked_sub(1) {
      Some(left) => {
        self.elements.set(left);
        Ok(())
      }
      None => Err(self.fail("more elements than the input can hold")),
    }
  }

  fn hint(&self, hint: Option<usize>) -> Option<usize> {
    hint.map(|n| n.min(self.input))
  }
}

/// One level of nesting, released on drop.
struct Depth<'b>(&'b Budget);

impl Drop for Depth<'_> {
  fn drop(&mut self) {
    self.0.depth.set(self.0.depth.get().saturating_sub(1));
  }
}

/// A deserializer that spends a [`Budget`].
struct Limited<'b, D> {
  inner: D,
  budget: &'b Budget,
}

/// A visitor that wraps every nested access in the budget.
struct Wrap<'b, V> {
  inner: V,
  budget: &'b Budget,
}

struct Seq<'b, A> {
  inner: A,
  budget: &'b Budget,
}

struct Map<'b, A> {
  inner: A,
  budget: &'b Budget,
}

struct Enum<'b, A> {
  inner: A,
  budget: &'b Budget,
}

struct Variant<'b, A> {
  inner: A,
  budget: &'b Budget,
}

struct Seed<'b, T> {
  inner: T,
  budget: &'b Budget,
}

impl<'b, V> Wrap<'b, V> {
  fn new(inner: V, budget: &'b Budget) -> Self {
    Self { inner, budget }
  }
}

macro_rules! forward_deserialize {
  ($($method:ident),* $(,)?) => {
    $(
      fn $method<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        self.inner.$method(Wrap::new(visitor, self.budget))
      }
    )*
  };
}

impl<'de, D: Deserializer<'de>> Deserializer<'de> for Limited<'_, D> {
  type Error = D::Error;

  forward_deserialize!(
    deserialize_any,
    deserialize_bool,
    deserialize_i8,
    deserialize_i16,
    deserialize_i32,
    deserialize_i64,
    deserialize_i128,
    deserialize_u8,
    deserialize_u16,
    deserialize_u32,
    deserialize_u64,
    deserialize_u128,
    deserialize_f32,
    deserialize_f64,
    deserialize_char,
    deserialize_str,
    deserialize_string,
    deserialize_bytes,
    deserialize_byte_buf,
    deserialize_option,
    deserialize_unit,
    deserialize_seq,
    deserialize_map,
    deserialize_identifier,
    deserialize_ignored_any,
  );

  fn deserialize_unit_struct<V: Visitor<'de>>(
    self,
    name: &'static str,
    visitor: V,
  ) -> Result<V::Value, Self::Error> {
    self
      .inner
      .deserialize_unit_struct(name, Wrap::new(visitor, self.budget))
  }

  fn deserialize_newtype_struct<V: Visitor<'de>>(
    self,
    name: &'static str,
    visitor: V,
  ) -> Result<V::Value, Self::Error> {
    self
      .inner
      .deserialize_newtype_struct(name, Wrap::new(visitor, self.budget))
  }

  fn deserialize_tuple<V: Visitor<'de>>(
    self,
    len: usize,
    visitor: V,
  ) -> Result<V::Value, Self::Error> {
    self
      .inner
      .deserialize_tuple(len, Wrap::new(visitor, self.budget))
  }

  fn deserialize_tuple_struct<V: Visitor<'de>>(
    self,
    name: &'static str,
    len: usize,
    visitor: V,
  ) -> Result<V::Value, Self::Error> {
    self
      .inner
      .deserialize_tuple_struct(name, len, Wrap::new(visitor, self.budget))
  }

  fn deserialize_struct<V: Visitor<'de>>(
    self,
    name: &'static str,
    fields: &'static [&'static str],
    visitor: V,
  ) -> Result<V::Value, Self::Error> {
    self
      .inner
      .deserialize_struct(name, fields, Wrap::new(visitor, self.budget))
  }

  fn deserialize_enum<V: Visitor<'de>>(
    self,
    name: &'static str,
    variants: &'static [&'static str],
    visitor: V,
  ) -> Result<V::Value, Self::Error> {
    self
      .inner
      .deserialize_enum(name, variants, Wrap::new(visitor, self.budget))
  }

  fn is_human_readable(&self) -> bool {
    self.inner.is_human_readable()
  }
}

macro_rules! forward_visit {
  ($($method:ident($ty:ty)),* $(,)?) => {
    $(
      fn $method<E: de::Error>(self, v: $ty) -> Result<Self::Value, E> {
        self.inner.$method(v)
      }
    )*
  };
}

impl<'de, V: Visitor<'de>> Visitor<'de> for Wrap<'_, V> {
  type Value = V::Value;

  fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    self.inner.expecting(f)
  }

  forward_visit!(
    visit_bool(bool),
    visit_i8(i8),
    visit_i16(i16),
    visit_i32(i32),
    visit_i64(i64),
    visit_i128(i128),
    visit_u8(u8),
    visit_u16(u16),
    visit_u32(u32),
    visit_u64(u64),
    visit_u128(u128),
    visit_f32(f32),
    visit_f64(f64),
    visit_char(char),
    visit_str(&str),
    visit_borrowed_str(&'de str),
    visit_string(String),
    visit_bytes(&[u8]),
    visit_borrowed_bytes(&'de [u8]),
    visit_byte_buf(Vec<u8>),
  );

  fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
    self.inner.visit_none()
  }

  fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
    self.inner.visit_unit()
  }

  fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
    let _depth = self.budget.enter::<D::Error>()?;
    self.inner.visit_some(Limited {
      inner: d,
      budget: self.budget,
    })
  }

  fn visit_newtype_struct<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
    let _depth = self.budget.enter::<D::Error>()?;
    self.inner.visit_newtype_struct(Limited {
      inner: d,
      budget: self.budget,
    })
  }

  fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
    let _depth = self.budget.enter::<A::Error>()?;
    self.inner.visit_seq(Seq {
      inner: seq,
      budget: self.budget,
    })
  }

  fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
    let _depth = self.budget.enter::<A::Error>()?;
    self.inner.visit_map(Map {
      inner: map,
      budget: self.budget,
    })
  }

  fn visit_enum<A: EnumAccess<'de>>(self, data: A) -> Result<Self::Value, A::Error> {
    let _depth = self.budget.enter::<A::Error>()?;
    self.inner.visit_enum(Enum {
      inner: data,
      budget: self.budget,
    })
  }
}

impl<'de, A: SeqAccess<'de>> SeqAccess<'de> for Seq<'_, A> {
  type Error = A::Error;

  fn next_element_seed<T: DeserializeSeed<'de>>(
    &mut self,
    seed: T,
  ) -> Result<Option<T::Value>, Self::Error> {
    self.budget.element::<A::Error>()?;
    self.inner.next_element_seed(Seed {
      inner: seed,
      budget: self.budget,
    })
  }

  fn size_hint(&self) -> Option<usize> {
    self.budget.hint(self.inner.size_hint())
  }
}

impl<'de, A: MapAccess<'de>> MapAccess<'de> for Map<'_, A> {
  type Error = A::Error;

  fn next_key_seed<K: DeserializeSeed<'de>>(
    &mut self,
    seed: K,
  ) -> Result<Option<K::Value>, Self::Error> {
    self.budget.element::<A::Error>()?;
    self.inner.next_key_seed(Seed {
      inner: seed,
      budget: self.budget,
    })
  }

  fn next_value_seed<T: DeserializeSeed<'de>>(&mut self, seed: T) -> Result<T::Value, Self::Error> {
    self.inner.next_value_seed(Seed {
      inner: seed,
      budget: self.budget,
    })
  }

  fn size_hint(&self) -> Option<usize> {
    self.budget.hint(self.inner.size_hint())
  }
}

impl<'de, 'b, A: EnumAccess<'de>> EnumAccess<'de> for Enum<'b, A> {
  type Error = A::Error;
  type Variant = Variant<'b, A::Variant>;

  fn variant_seed<T: DeserializeSeed<'de>>(
    self,
    seed: T,
  ) -> Result<(T::Value, Self::Variant), Self::Error> {
    let budget = self.budget;
    self
      .inner
      .variant_seed(Seed {
        inner: seed,
        budget,
      })
      .map(|(value, variant)| {
        (
          value,
          Variant {
            inner: variant,
            budget,
          },
        )
      })
  }
}

impl<'de, A: VariantAccess<'de>> VariantAccess<'de> for Variant<'_, A> {
  type Error = A::Error;

  fn unit_variant(self) -> Result<(), Self::Error> {
    self.inner.unit_variant()
  }

  fn newtype_variant_seed<T: DeserializeSeed<'de>>(self, seed: T) -> Result<T::Value, Self::Error> {
    self.inner.newtype_variant_seed(Seed {
      inner: seed,
      budget: self.budget,
    })
  }

  fn tuple_variant<V: Visitor<'de>>(self, len: usize, visitor: V) -> Result<V::Value, Self::Error> {
    self
      .inner
      .tuple_variant(len, Wrap::new(visitor, self.budget))
  }

  fn struct_variant<V: Visitor<'de>>(
    self,
    fields: &'static [&'static str],
    visitor: V,
  ) -> Result<V::Value, Self::Error> {
    self
      .inner
      .struct_variant(fields, Wrap::new(visitor, self.budget))
  }
}

impl<'de, T: DeserializeSeed<'de>> DeserializeSeed<'de> for Seed<'_, T> {
  type Value = T::Value;

  fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
    self.inner.deserialize(Limited {
      inner: d,
      budget: self.budget,
    })
  }
}

/// Decode a `T` from `bytes` of at most `limit` bytes, bounded as the module doc describes.
///
/// # Errors
///
/// [`CodecError::TooLarge`] before decoding when `bytes` is over `limit`;
/// [`CodecError::Decode`] when the bytes are not exactly one `T` or exceed a bound.
pub fn decode_limited<T: DeserializeOwned>(bytes: &[u8], limit: usize) -> Result<T, CodecError> {
  if bytes.len() > limit {
    return Err(CodecError::TooLarge {
      size: bytes.len(),
      limit,
    });
  }
  let budget = Budget::new(bytes.len());
  let mut de = ::postcard::Deserializer::from_bytes(bytes);
  let value = T::deserialize(Limited {
    inner: &mut de,
    budget: &budget,
  })
  .map_err(|e| {
    CodecError::Decode(
      budget
        .why
        .get()
        .map_or_else(|| e.to_string(), str::to_owned),
    )
  })?;
  let rest = de
    .finalize()
    .map_err(|e| CodecError::Decode(e.to_string()))?;
  if !rest.is_empty() {
    return Err(CodecError::Decode(format!(
      "{} trailing bytes after the value",
      rest.len()
    )));
  }
  Ok(value)
}

// ---------------------------------------------------------------------------------------------
// Envelopes
// ---------------------------------------------------------------------------------------------

/// Nested bytes on the wire: a postcard byte string, decoded without preallocation.
struct Bytes(Vec<u8>);

/// Borrowed [`Bytes`] for encoding.
struct BytesRef<'a>(&'a [u8]);

impl Serialize for BytesRef<'_> {
  fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_bytes(self.0)
  }
}

impl<'de> Deserialize<'de> for Bytes {
  fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
    struct BytesVisitor;

    impl Visitor<'_> for BytesVisitor {
      type Value = Bytes;

      fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a byte string")
      }

      fn visit_bytes<E: de::Error>(self, v: &[u8]) -> Result<Bytes, E> {
        Ok(Bytes(v.to_vec()))
      }
    }

    d.deserialize_byte_buf(BytesVisitor)
  }
}

/// The postcard index of a [`FrameKind`] (design note §5; fixed, never reordered).
pub fn frame_kind_index(kind: FrameKind) -> u8 {
  match kind {
    FrameKind::Call => 0,
    FrameKind::Item => 1,
    FrameKind::End => 2,
    FrameKind::Exit => 3,
    FrameKind::Cancel => 4,
    FrameKind::Credit => 5,
    FrameKind::Hello => 6,
    FrameKind::Resume => 7,
    FrameKind::Ping => 8,
    FrameKind::Pong => 9,
  }
}

/// The [`FrameKind`] of a postcard index.
pub fn frame_kind_from_index(index: u8) -> Option<FrameKind> {
  Some(match index {
    0 => FrameKind::Call,
    1 => FrameKind::Item,
    2 => FrameKind::End,
    3 => FrameKind::Exit,
    4 => FrameKind::Cancel,
    5 => FrameKind::Credit,
    6 => FrameKind::Hello,
    7 => FrameKind::Resume,
    8 => FrameKind::Ping,
    9 => FrameKind::Pong,
    _ => return None,
  })
}

#[derive(Serialize)]
struct FrameOut<'a> {
  id: CallId,
  kind: u8,
  flags: u8,
  tag: Option<&'a str>,
  v: Option<u32>,
  cursor: Option<Cursor>,
  credits: Option<u32>,
  payload: Option<BytesRef<'a>>,
}

#[derive(Deserialize)]
struct FrameIn {
  id: CallId,
  kind: u8,
  flags: u8,
  tag: Option<String>,
  v: Option<u32>,
  cursor: Option<Cursor>,
  credits: Option<u32>,
  payload: Option<Bytes>,
}

fn check_tag(tag: &str) -> Result<(), CodecError> {
  if is_valid_tag(tag) {
    Ok(())
  } else {
    Err(CodecError::Decode("invalid procedure tag".to_owned()))
  }
}

fn checked_size(bytes: Vec<u8>) -> Result<Vec<u8>, CodecError> {
  if bytes.len() > MAX_FRAME_BYTES {
    return Err(CodecError::TooLarge {
      size: bytes.len(),
      limit: MAX_FRAME_BYTES,
    });
  }
  Ok(bytes)
}

/// Encode a frame whose payload is already postcard bytes.
///
/// # Errors
///
/// [`CodecError::TooLarge`] when the frame is over [`MAX_FRAME_BYTES`].
pub fn encode_frame(frame: &Frame<Vec<u8>>) -> Result<Vec<u8>, CodecError> {
  let out = FrameOut {
    id: frame.id,
    kind: frame_kind_index(frame.kind),
    flags: frame.flags.0,
    tag: frame.tag.as_deref(),
    v: frame.v,
    cursor: frame.cursor,
    credits: frame.credits,
    payload: frame.payload.as_deref().map(BytesRef),
  };
  checked_size(Postcard::encode(&out)?)
}

/// Decode one frame; its payload stays postcard bytes for the procedure's typed bridge.
///
/// The entry point of the `rpc_postcard_frame` fuzz target: bounded as the module doc says;
/// refuses an unknown kind, reserved flag bits, an invalid tag and a malformed cursor.
///
/// # Errors
///
/// [`CodecError::TooLarge`] before decoding; [`CodecError::Decode`] otherwise.
pub fn decode_frame(bytes: &[u8]) -> Result<Frame<Vec<u8>>, CodecError> {
  let raw: FrameIn = Postcard::decode(bytes)?;
  let kind = frame_kind_from_index(raw.kind)
    .ok_or_else(|| CodecError::Decode(format!("unknown frame kind {}", raw.kind)))?;
  let flags = FrameFlags(raw.flags);
  if flags.has_reserved() {
    return Err(CodecError::Decode(
      "reserved frame flag bits set".to_owned(),
    ));
  }
  if let Some(tag) = &raw.tag {
    check_tag(tag)?;
  }
  Ok(Frame {
    id: raw.id,
    kind,
    flags,
    tag: raw.tag,
    v: raw.v,
    cursor: raw.cursor,
    credits: raw.credits,
    payload: raw.payload.map(|p| p.0),
  })
}

#[derive(Serialize)]
enum ExitOut<'a> {
  Success(BytesRef<'a>),
  Failure(BytesRef<'a>),
}

#[derive(Deserialize)]
enum ExitIn {
  Success(Bytes),
  Failure(Bytes),
}

fn exit_out<R>(
  exit: &WireExit<Vec<u8>, Value>,
  with: impl FnOnce(ExitOut<'_>) -> Result<R, CodecError>,
) -> Result<R, CodecError> {
  match exit {
    WireExit::Success { value } => with(ExitOut::Success(BytesRef(value))),
    WireExit::Failure { error } => {
      let text = serde_json::to_vec(error).map_err(|e| CodecError::Encode(e.to_string()))?;
      with(ExitOut::Failure(BytesRef(&text)))
    }
  }
}

fn exit_in(exit: ExitIn) -> Result<WireExit<Vec<u8>, Value>, CodecError> {
  Ok(match exit {
    ExitIn::Success(value) => WireExit::Success { value: value.0 },
    ExitIn::Failure(text) => {
      let error: Value =
        serde_json::from_slice(&text.0).map_err(|e| CodecError::Decode(e.to_string()))?;
      if !error.get("_tag").is_some_and(Value::is_string) {
        return Err(CodecError::Decode(
          "a failure is a tagged union `{\"_tag\": …}`".to_owned(),
        ));
      }
      WireExit::Failure { error }
    }
  })
}

/// Encode an exit: the success value as the output's postcard bytes, a failure's error union
/// as JSON text.
///
/// # Errors
///
/// [`CodecError::TooLarge`] when over [`MAX_FRAME_BYTES`].
pub fn encode_exit(exit: &WireExit<Vec<u8>, Value>) -> Result<Vec<u8>, CodecError> {
  exit_out(exit, |out| checked_size(Postcard::encode(&out)?))
}

/// Decode an exit (the client decodes a success's bytes as its output type).
///
/// # Errors
///
/// [`CodecError::TooLarge`] before decoding; [`CodecError::Decode`] otherwise, also for a
/// failure that is not a tagged union.
pub fn decode_exit(bytes: &[u8]) -> Result<WireExit<Vec<u8>, Value>, CodecError> {
  exit_in(Postcard::decode(bytes)?)
}

#[derive(Serialize)]
struct CallOut<'a> {
  id: Option<CallId>,
  tag: &'a str,
  v: Option<u32>,
  payload: BytesRef<'a>,
}

#[derive(Deserialize)]
struct CallIn {
  id: Option<CallId>,
  tag: String,
  v: Option<u32>,
  payload: Bytes,
}

/// Encode a call envelope (`POST /rpc` with the postcard media type).
///
/// # Errors
///
/// [`CodecError::TooLarge`] when over [`MAX_FRAME_BYTES`].
pub fn encode_call(call: &WireCall<Vec<u8>>) -> Result<Vec<u8>, CodecError> {
  checked_size(Postcard::encode(&CallOut {
    id: call.id,
    tag: &call.tag,
    v: call.v,
    payload: BytesRef(&call.payload),
  })?)
}

/// Decode a call envelope; the payload stays postcard bytes for the procedure's typed bridge.
///
/// # Errors
///
/// [`CodecError::TooLarge`] before decoding; [`CodecError::Decode`] otherwise, also for an
/// invalid tag.
pub fn decode_call(bytes: &[u8]) -> Result<WireCall<Vec<u8>>, CodecError> {
  let raw: CallIn = Postcard::decode(bytes)?;
  check_tag(&raw.tag)?;
  Ok(WireCall {
    id: raw.id,
    tag: raw.tag,
    v: raw.v,
    payload: raw.payload.0,
  })
}

#[derive(Serialize)]
struct ResultOut<'a> {
  id: Option<CallId>,
  tag: &'a str,
  exit: ExitOut<'a>,
}

#[derive(Deserialize)]
struct ResultIn {
  id: Option<CallId>,
  tag: String,
  exit: ExitIn,
}

/// Encode a call result.
///
/// # Errors
///
/// [`CodecError::TooLarge`] when over [`MAX_FRAME_BYTES`].
pub fn encode_result(result: &WireResult<Vec<u8>, Value>) -> Result<Vec<u8>, CodecError> {
  exit_out(&result.exit, |exit| {
    checked_size(Postcard::encode(&ResultOut {
      id: result.id,
      tag: &result.tag,
      exit,
    })?)
  })
}

/// Decode a call result.
///
/// # Errors
///
/// As [`decode_exit`], also for an invalid tag.
pub fn decode_result(bytes: &[u8]) -> Result<WireResult<Vec<u8>, Value>, CodecError> {
  let raw: ResultIn = Postcard::decode(bytes)?;
  check_tag(&raw.tag)?;
  Ok(WireResult {
    id: raw.id,
    tag: raw.tag,
    exit: exit_in(raw.exit)?,
  })
}

// ---------------------------------------------------------------------------------------------
// Records (stream bodies, WebTransport and QUIC streams)
// ---------------------------------------------------------------------------------------------

/// One length-prefixed record: the u32 big-endian length of `frame`, then `frame`.
///
/// # Errors
///
/// [`CodecError::TooLarge`] when `frame` is over [`MAX_FRAME_BYTES`].
pub fn encode_record(frame: &[u8]) -> Result<Vec<u8>, CodecError> {
  let len = u32::try_from(frame.len())
    .ok()
    .filter(|_| frame.len() <= MAX_FRAME_BYTES)
    .ok_or(CodecError::TooLarge {
      size: frame.len(),
      limit: MAX_FRAME_BYTES,
    })?;
  let mut out = Vec::with_capacity(RECORD_HEADER_BYTES + frame.len());
  out.extend_from_slice(&len.to_be_bytes());
  out.extend_from_slice(frame);
  Ok(out)
}

/// The first complete record of `buf` and the bytes it took, `None` while incomplete.
///
/// # Errors
///
/// [`CodecError::TooLarge`] as soon as the length prefix announces more than
/// [`MAX_FRAME_BYTES`], before the record's bytes are buffered.
pub fn split_record(buf: &[u8]) -> Result<Option<(&[u8], usize)>, CodecError> {
  let Some((header, rest)) = buf.split_first_chunk::<RECORD_HEADER_BYTES>() else {
    return Ok(None);
  };
  let len = u32::from_be_bytes(*header) as usize;
  if len > MAX_FRAME_BYTES {
    return Err(CodecError::TooLarge {
      size: len,
      limit: MAX_FRAME_BYTES,
    });
  }
  Ok(
    rest
      .get(..len)
      .map(|record| (record, RECORD_HEADER_BYTES + len)),
  )
}

// ---------------------------------------------------------------------------------------------
// Transport bridges
// ---------------------------------------------------------------------------------------------

/// The handler's value of a client `call` or `item` frame payload, through the entry's input
/// bridge; an absent payload (the `call` of a client stream or bidi) is `null`.
///
/// # Errors
///
/// [`CodecError::Unsupported`] when the procedure has no typed bridge (answer 415, or refuse
/// the call frame); [`CodecError::Decode`] when the bytes are not the input type.
pub fn decode_client_payload(
  codecs: &ProcedureCodecs,
  payload: Option<&[u8]>,
) -> Result<Value, CodecError> {
  match payload {
    None => Ok(Value::Null),
    Some(bytes) => codecs.decode_input(CodecId::Postcard, bytes),
  }
}

/// The postcard frame of a server frame of a running call ([`crate::kinds::CallFrames`]):
/// an `item` through the entry's output bridge, an `exit` through
/// [`ProcedureCodecs::encode_exit`], a `hello` as [`Hello`]; other kinds carry no payload.
///
/// # Errors
///
/// [`CodecError::Unsupported`] without a typed bridge, [`CodecError::Encode`] for a payload
/// that is not of its kind's shape, [`CodecError::TooLarge`] over [`MAX_FRAME_BYTES`].
pub fn encode_server_frame(codecs: &ProcedureCodecs, frame: &Frame) -> Result<Vec<u8>, CodecError> {
  let codec = CodecId::Postcard;
  let encode_err = |e: serde_json::Error| CodecError::Encode(e.to_string());
  let payload = match (frame.kind, &frame.payload) {
    (_, None) => None,
    (FrameKind::Item, Some(value)) => Some(codecs.encode_output(codec, value)?),
    (FrameKind::Exit, Some(value)) => {
      let exit: WireExit = serde_json::from_value(value.clone()).map_err(encode_err)?;
      Some(encode_exit(&codecs.encode_exit(codec, &exit)?)?)
    }
    (FrameKind::Hello, Some(value)) => {
      let hello: Hello = serde_json::from_value(value.clone()).map_err(encode_err)?;
      Some(Postcard::encode(&hello)?)
    }
    (kind, Some(_)) => {
      return Err(CodecError::Encode(format!(
        "a server `{}` frame carries no payload",
        frame_kind_index(kind)
      )));
    }
  };
  encode_frame(&Frame {
    id: frame.id,
    kind: frame.kind,
    flags: frame.flags,
    tag: frame.tag.clone(),
    v: frame.v,
    cursor: frame.cursor,
    credits: frame.credits,
    payload,
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::wire::{Resume, ResumeCall, SessionId};
  use serde_json::json;

  fn id(n: u64) -> CallId {
    CallId::new(n).unwrap()
  }

  #[test]
  fn names_and_codec_id() {
    assert_eq!(MEDIA_TYPE, "application/vnd.id-effect.rpc+postcard");
    assert_eq!(SUBPROTOCOL, "idrpc.postcard.v1");
    assert!(STREAM_MEDIA_TYPE.starts_with(MEDIA_TYPE));
    let codec = CodecId::Postcard;
    assert_eq!(Postcard::ID, codec);
    assert_eq!(codec.name(), "postcard");
    assert_eq!(codec.content_type(), MEDIA_TYPE);
    assert_eq!(codec.subprotocol(), SUBPROTOCOL);
    assert!(!codec.is_self_describing());
    assert_eq!(CodecId::ALL, [CodecId::Json, CodecId::Postcard]);
    assert_eq!(
      super::super::negotiate_content_type(Some("application/vnd.id-effect.rpc+postcard")),
      Ok(codec)
    );
    assert_eq!(
      super::super::negotiate_subprotocol(["idrpc.postcard.v1", "idrpc.json.v1"]),
      Some(codec)
    );
    assert_eq!(
      super::super::negotiate_accept(Some(MEDIA_TYPE), CodecId::Json),
      Ok(codec)
    );
    let bytes = codec.encode(&(7_u8, "x")).unwrap();
    assert_eq!(bytes, [7, 1, b'x']);
    assert_eq!(
      codec.decode::<(u8, String)>(&bytes).unwrap(),
      (7, "x".into())
    );
  }

  #[test]
  fn frame_kind_indices_are_fixed() {
    let kinds = [
      FrameKind::Call,
      FrameKind::Item,
      FrameKind::End,
      FrameKind::Exit,
      FrameKind::Cancel,
      FrameKind::Credit,
      FrameKind::Hello,
      FrameKind::Resume,
      FrameKind::Ping,
      FrameKind::Pong,
    ];
    for (index, kind) in kinds.into_iter().enumerate() {
      let index = u8::try_from(index).unwrap();
      assert_eq!(frame_kind_index(kind), index);
      assert_eq!(frame_kind_from_index(index), Some(kind));
    }
    assert_eq!(frame_kind_from_index(10), None);
    // The kind byte sits right after the id varint.
    let bytes = encode_frame(&Frame::new(id(1), FrameKind::Pong)).unwrap();
    assert_eq!(bytes, [1, 9, 0, 0, 0, 0, 0, 0]);
  }

  #[test]
  fn frames_round_trip_and_bad_frames_are_refused() {
    let frame = Frame::new(id(3), FrameKind::Item)
      .with_flags(FrameFlags::END.with(FrameFlags::ACK))
      .with_cursor(Cursor::events(9))
      .with_payload(vec![1, 2, 3]);
    let bytes = encode_frame(&frame).unwrap();
    assert_eq!(decode_frame(&bytes).unwrap(), frame);
    let credit = Frame::credit(id(3), 32);
    assert_eq!(
      decode_frame(&encode_frame(&credit).unwrap()).unwrap(),
      credit
    );

    let mut unknown_kind = encode_frame(&Frame::new(id(1), FrameKind::Ping)).unwrap();
    unknown_kind[1] = 10;
    let err = decode_frame(&unknown_kind).unwrap_err();
    assert_eq!(err, CodecError::Decode("unknown frame kind 10".into()));

    let reserved = encode_frame(&Frame::new(id(1), FrameKind::End).with_flags(FrameFlags(4)));
    let err = decode_frame(&reserved.unwrap()).unwrap_err();
    assert!(err.to_string().contains("reserved"), "{err}");

    let bad_tag = encode_frame(&Frame::call(id(1), "no spaces", Vec::new())).unwrap();
    assert!(
      decode_frame(&bad_tag)
        .unwrap_err()
        .to_string()
        .contains("tag")
    );

    let too_big_id = [0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x10, 0, 0];
    assert!(decode_frame(&too_big_id).is_err());

    let mut trailing = encode_frame(&Frame::new(id(1), FrameKind::Ping)).unwrap();
    trailing.push(0);
    let err = decode_frame(&trailing).unwrap_err();
    assert!(err.to_string().contains("trailing"), "{err}");

    assert!(decode_frame(&[]).is_err());
  }

  #[test]
  fn oversized_input_is_refused_before_decoding() {
    let huge = vec![0_u8; MAX_FRAME_BYTES + 1];
    assert_eq!(
      decode_frame(&huge).unwrap_err(),
      CodecError::TooLarge {
        size: MAX_FRAME_BYTES + 1,
        limit: MAX_FRAME_BYTES
      }
    );
    let big = Frame::new(id(1), FrameKind::Item).with_payload(vec![0; MAX_FRAME_BYTES]);
    assert!(matches!(
      encode_frame(&big),
      Err(CodecError::TooLarge { .. })
    ));
    assert!(matches!(
      decode_limited::<u8>(&[1, 2], 1),
      Err(CodecError::TooLarge { size: 2, limit: 1 })
    ));
  }

  #[derive(Debug, Serialize, Deserialize, PartialEq)]
  enum Tree {
    Leaf,
    Node(Box<Tree>),
  }

  #[test]
  fn nesting_and_element_counts_are_bounded() {
    let mut deep = vec![1_u8; 200];
    deep.push(0);
    let err = Postcard::decode::<Tree>(&deep).unwrap_err();
    assert_eq!(
      err,
      CodecError::Decode("nesting deeper than the limit".into())
    );
    let mut shallow = vec![1_u8; 10];
    shallow.push(0);
    assert!(Postcard::decode::<Tree>(&shallow).is_ok());

    // A billion unit values announced in five bytes.
    let units = Postcard::encode(&1_000_000_000_u64).unwrap();
    let err = Postcard::decode::<Vec<()>>(&units).unwrap_err();
    assert_eq!(
      err,
      CodecError::Decode("more elements than the input can hold".into())
    );
    // A huge length prefix does not preallocate: it runs out of input.
    let long = Postcard::encode(&(1_u64 << 40)).unwrap();
    assert!(Postcard::decode::<Vec<u64>>(&long).is_err());
    assert!(Postcard::decode::<String>(&long).is_err());
    let map: std::collections::BTreeMap<String, Option<u8>> =
      [("a".to_owned(), Some(1)), ("b".to_owned(), None)].into();
    let bytes = Postcard::encode(&map).unwrap();
    assert_eq!(
      Postcard::decode::<std::collections::BTreeMap<String, Option<u8>>>(&bytes).unwrap(),
      map
    );
    let units = Postcard::encode(&1_000_000_000_u64).unwrap();
    assert!(Postcard::decode::<std::collections::BTreeMap<u8, ()>>(&units).is_err());
  }

  #[derive(Debug, Serialize, Deserialize, PartialEq)]
  struct Unit;

  #[derive(Debug, Serialize, Deserialize, PartialEq)]
  struct Wrapper(u8);

  #[derive(Debug, Serialize, Deserialize, PartialEq)]
  struct Pair(u8, u16);

  #[test]
  fn the_budget_forwards_to_any_deserializer() {
    // Tuple structs, hash maps (size hints) and types that ask `is_human_readable`.
    type More = (Pair, std::collections::HashMap<u8, bool>, std::net::IpAddr);
    let value: More = (
      Pair(1, 300),
      [(1, true), (2, false)].into(),
      std::net::IpAddr::from([10, 0, 0, 1]),
    );
    let bytes = Postcard::encode(&value).unwrap();
    assert_eq!(Postcard::decode::<More>(&bytes).unwrap(), value);

    // With a self-describing deserializer below, type errors name what was expected.
    let budget = Budget::new(8);
    let mut json = serde_json::Deserializer::from_str("1");
    let err = Bytes::deserialize(Limited {
      inner: &mut json,
      budget: &budget,
    })
    .err()
    .unwrap();
    assert!(err.to_string().contains("a byte string"), "{err}");
    let mut json = serde_json::Deserializer::from_str("\"x\"");
    let err = u8::deserialize(Limited {
      inner: &mut json,
      budget: &budget,
    })
    .unwrap_err();
    assert!(err.to_string().contains("u8"), "{err}");
  }

  #[derive(Debug, Serialize, Deserialize, PartialEq)]
  enum Shapes {
    Pair(u8, char),
    Named { a: i16, b: i32 },
    Wide(i128, u128),
  }

  #[test]
  fn every_serde_shape_passes_through_the_budget() {
    type All = (
      bool,
      (i8, i16, i32, i64),
      (u16, u32, u64, f32, f64),
      (Unit, Wrapper, Shapes, Shapes, Shapes),
      Vec<u8>,
      (),
    );
    let value: All = (
      true,
      (-1, -2, -3, -4),
      (5, 6, 7, 1.5, 2.5),
      (
        Unit,
        Wrapper(3),
        Shapes::Pair(1, 'é'),
        Shapes::Named { a: -5, b: 6 },
        Shapes::Wide(-7, 8),
      ),
      vec![9, 10],
      (),
    );
    let bytes = Postcard::encode(&value).unwrap();
    assert_eq!(Postcard::decode::<All>(&bytes).unwrap(), value);
    // Borrowed strings and bytes go through `visit_borrowed_*`.
    let text = Postcard::encode("hé").unwrap();
    assert_eq!(Postcard::decode::<Box<str>>(&text).unwrap().as_ref(), "hé");
    let blob = Postcard::encode(&BytesRef(&[1, 2])).unwrap();
    assert_eq!(Postcard::decode::<Bytes>(&blob).unwrap().0, [1, 2]);
    // Self-describing input is refused (postcard has no `deserialize_any`).
    assert!(Postcard::decode::<Value>(&bytes).is_err());
    assert!(Postcard::decode::<serde::de::IgnoredAny>(&bytes).is_err());
  }

  #[test]
  fn calls_results_and_exits_round_trip() {
    let call = WireCall::new("Users.add", vec![1, 2])
      .with_id(id(4))
      .with_version(2);
    assert_eq!(decode_call(&encode_call(&call).unwrap()).unwrap(), call);
    let bad = encode_call(&WireCall::new("bad tag", Vec::new())).unwrap();
    assert!(decode_call(&bad).is_err());

    let success = WireExit::Success { value: vec![7] };
    assert_eq!(
      decode_exit(&encode_exit(&success).unwrap()).unwrap(),
      success
    );
    let failure = WireExit::Failure {
      error: json!({"_tag": "NotFound", "id": 3}),
    };
    assert_eq!(
      decode_exit(&encode_exit(&failure).unwrap()).unwrap(),
      failure
    );

    let result = WireResult::new(Some(id(4)), "Users.add", failure.clone());
    assert_eq!(
      decode_result(&encode_result(&result).unwrap()).unwrap(),
      result
    );
    let bad = encode_result(&WireResult::new(None, "bad tag", success)).unwrap();
    assert!(decode_result(&bad).is_err());

    // A failure must be a tagged union, and valid JSON.
    let untagged = encode_exit(&WireExit::Failure { error: json!(1) }).unwrap();
    assert!(
      decode_exit(&untagged)
        .unwrap_err()
        .to_string()
        .contains("tagged")
    );
    let not_json = Postcard::encode(&ExitOut::Failure(BytesRef(b"{"))).unwrap();
    assert!(matches!(decode_exit(&not_json), Err(CodecError::Decode(_))));
    let big = WireExit::Failure {
      error: json!({"_tag": "X", "s": "x".repeat(MAX_FRAME_BYTES)}),
    };
    assert!(matches!(
      encode_exit(&big),
      Err(CodecError::TooLarge { .. })
    ));
  }

  #[test]
  fn records_are_length_prefixed_and_capped_early() {
    let record = encode_record(b"abc").unwrap();
    assert_eq!(record, [0, 0, 0, 3, b'a', b'b', b'c']);
    let mut buf = record.clone();
    buf.extend_from_slice(&[0, 0]);
    assert_eq!(split_record(&buf).unwrap(), Some((&b"abc"[..], 7)));
    assert_eq!(split_record(&record[..5]).unwrap(), None);
    assert_eq!(split_record(&[0, 0]).unwrap(), None);
    let announced = u32::try_from(MAX_FRAME_BYTES + 1).unwrap().to_be_bytes();
    assert!(matches!(
      split_record(&announced),
      Err(CodecError::TooLarge { .. })
    ));
    assert!(matches!(
      encode_record(&vec![0; MAX_FRAME_BYTES + 1]),
      Err(CodecError::TooLarge { .. })
    ));
  }

  #[test]
  fn server_frames_go_through_the_bridges() {
    use crate::contract::ProcedureKind;
    use crate::handler::WireSchema;
    use id_effect::schema;

    let text = WireSchema::new(schema::string::<()>()).with_serde();
    let codecs = ProcedureCodecs::new(
      ProcedureKind::ServerStream,
      text.typed_codec().cloned(),
      text.typed_codec().cloned(),
    );
    let item = Frame::new(id(2), FrameKind::Item).with_payload(json!("hi"));
    let back = decode_frame(&encode_server_frame(&codecs, &item).unwrap()).unwrap();
    let hi: String = Postcard::decode(back.payload.as_deref().unwrap()).unwrap();
    assert_eq!(hi, "hi");

    let exit =
      Frame::new(id(2), FrameKind::Exit).with_payload(json!({"_tag": "Success", "value": null}));
    let back = decode_frame(&encode_server_frame(&codecs, &exit).unwrap()).unwrap();
    assert_eq!(
      decode_exit(back.payload.as_deref().unwrap()).unwrap(),
      WireExit::Success { value: Vec::new() }
    );

    let session = SessionId::from([7; 16]);
    let hello = Hello {
      session,
      codec: SUBPROTOCOL.to_owned(),
      max_frame: 65_536,
      window: 32,
    };
    let frame = Frame::new(CallId::CONNECTION, FrameKind::Hello)
      .with_payload(serde_json::to_value(&hello).unwrap());
    let back = decode_frame(&encode_server_frame(&codecs, &frame).unwrap()).unwrap();
    assert_eq!(
      Postcard::decode::<Hello>(back.payload.as_deref().unwrap()).unwrap(),
      hello
    );
    let resume = Resume {
      session,
      calls: vec![ResumeCall {
        id: id(3),
        cursor: Cursor::sequence(4),
      }],
    };
    let bytes = Postcard::encode(&resume).unwrap();
    assert_eq!(Postcard::decode::<Resume>(&bytes).unwrap(), resume);

    let end = Frame::new(id(2), FrameKind::End).with_flags(FrameFlags::END);
    let back = decode_frame(&encode_server_frame(&codecs, &end).unwrap()).unwrap();
    assert_eq!(back.payload, None);
    let odd = Frame::new(id(2), FrameKind::Credit).with_payload(json!(1));
    assert!(matches!(
      encode_server_frame(&codecs, &odd),
      Err(CodecError::Encode(_))
    ));
    let bad_exit = Frame::new(id(2), FrameKind::Exit).with_payload(json!(1));
    assert!(encode_server_frame(&codecs, &bad_exit).is_err());
    let bad_hello = Frame::new(id(0), FrameKind::Hello).with_payload(json!(1));
    assert!(encode_server_frame(&codecs, &bad_hello).is_err());

    assert_eq!(decode_client_payload(&codecs, None).unwrap(), Value::Null);
    let bytes = Postcard::encode("yo").unwrap();
    assert_eq!(
      decode_client_payload(&codecs, Some(&bytes)).unwrap(),
      json!("yo")
    );
    let untyped = ProcedureCodecs::default();
    assert!(matches!(
      decode_client_payload(&untyped, Some(&bytes)),
      Err(CodecError::Unsupported(_))
    ));
    assert!(matches!(
      encode_server_frame(&untyped, &item),
      Err(CodecError::Unsupported(_))
    ));
  }
}
