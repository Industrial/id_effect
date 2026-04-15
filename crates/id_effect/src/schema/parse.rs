//! **Stratum 13 — Data & Schema** — parse module.
//!
//! Bidirectional codecs with path-aware [`ParseError`] (`id_effect::schema`, §27).
//!
//! [`Schema`] carries synchronous `decode` / `encode` plus [`Schema::decode_unknown`] for
//! tree-shaped [`Unknown`] input (e.g. config or JSON-shaped values without pulling in serde).

use std::collections::BTreeMap;
use std::marker::PhantomData;
use std::sync::Arc;

use crate::schema::data::EffectData;

/// Parse failure with a dot-separated field path (empty = root).
#[derive(Clone, Debug, crate::EffectData)]
pub struct ParseError {
  /// Dot-separated field path (empty at root).
  pub path: String,
  /// Human-readable failure reason.
  pub message: String,
}

impl ParseError {
  /// Build an error at `path` with `message`.
  pub fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
    Self {
      path: path.into(),
      message: message.into(),
    }
  }

  /// Prefix a parent segment (`user` + `age` → `user.age`).
  pub fn prefix(self, segment: &str) -> Self {
    let path = if self.path.is_empty() {
      segment.to_string()
    } else {
      format!("{segment}.{}", self.path)
    };
    Self {
      path,
      message: self.message,
    }
  }

  /// Prefix an array index (segment should be decimal digits).
  pub fn prefix_index(self, idx: usize) -> Self {
    self.prefix(&idx.to_string())
  }
}

/// Dynamically structured input for [`Schema::decode_unknown`].
#[derive(Clone, Debug, PartialEq)]
pub enum Unknown {
  /// JSON-like null.
  Null,
  /// Boolean.
  Bool(bool),
  /// Signed integer (wire for numbers without float).
  I64(i64),
  /// IEEE-754 double (JSON numbers that are not integers, or explicitly floating).
  F64(f64),
  /// UTF-8 string.
  String(String),
  /// Ordered list of nested values.
  Array(Vec<Unknown>),
  /// String-keyed map of nested values.
  Object(BTreeMap<String, Unknown>),
}

type BoxDecode<I, A> = Arc<dyn Fn(I) -> Result<A, ParseError> + Send + Sync>;
type BoxEncode<A, I> = Arc<dyn Fn(A) -> I + Send + Sync>;
type BoxDecodeUnknown<A> = Arc<dyn Fn(&Unknown) -> Result<A, ParseError> + Send + Sync>;

/// Bidirectional schema: semantic `A`, wire/intermediate `I`, phantom `E` ([`EffectData`] tag).
pub struct Schema<A, I, E = ()> {
  phantom: PhantomData<fn() -> E>,
  decode: BoxDecode<I, A>,
  encode: BoxEncode<A, I>,
  decode_unknown: BoxDecodeUnknown<A>,
}

impl<A, I, E> Clone for Schema<A, I, E> {
  fn clone(&self) -> Self {
    Self {
      phantom: PhantomData,
      decode: self.decode.clone(),
      encode: self.encode.clone(),
      decode_unknown: self.decode_unknown.clone(),
    }
  }
}

impl<A, I, E> Schema<A, I, E> {
  /// Decode wire value `input` to `A`.
  pub fn decode(&self, input: I) -> Result<A, ParseError> {
    (self.decode)(input)
  }

  /// Encode semantic `value` to wire `I`.
  pub fn encode(&self, value: A) -> I {
    (self.encode)(value)
  }

  /// Decode from dynamic [`Unknown`] (e.g. config trees).
  pub fn decode_unknown(&self, input: &Unknown) -> Result<A, ParseError> {
    (self.decode_unknown)(input)
  }
}

impl<A: 'static, I: 'static, E: EffectData + 'static> Schema<A, I, E> {
  /// Construct a schema from decode / encode / `decode_unknown` closures.
  pub fn make(
    decode: impl Fn(I) -> Result<A, ParseError> + Send + Sync + 'static,
    encode: impl Fn(A) -> I + Send + Sync + 'static,
    decode_unknown: impl Fn(&Unknown) -> Result<A, ParseError> + Send + Sync + 'static,
  ) -> Self {
    Self {
      phantom: PhantomData,
      decode: Arc::new(decode),
      encode: Arc::new(encode),
      decode_unknown: Arc::new(decode_unknown),
    }
  }
}

/// `i64` schema; wire type is `i64`.
pub fn i64<E: EffectData + 'static>() -> Schema<i64, i64, E> {
  Schema::make(
    Ok,
    |n| n,
    |u| match u {
      Unknown::I64(n) => Ok(*n),
      Unknown::Null => Err(ParseError::new("", "expected i64, got null")),
      _ => Err(ParseError::new("", "expected i64")),
    },
  )
}

/// `i64` with [`Unknown`] as the wire type (for [`crate::schema::extra::union_chain`] and similar).
pub fn i64_unknown_wire<E: EffectData + 'static>() -> Schema<i64, Unknown, E> {
  Schema::make(
    |u: Unknown| match u {
      Unknown::I64(n) => Ok(n),
      Unknown::Null => Err(ParseError::new("", "expected i64, got null")),
      _ => Err(ParseError::new("", "expected i64")),
    },
    |n: i64| Unknown::I64(n),
    |u| match u {
      Unknown::I64(n) => Ok(*n),
      Unknown::Null => Err(ParseError::new("", "expected i64, got null")),
      _ => Err(ParseError::new("", "expected i64")),
    },
  )
}

/// UTF-8 string schema; wire type is [`String`].
pub fn string<E: EffectData + 'static>() -> Schema<String, String, E> {
  Schema::make(
    Ok,
    |s| s,
    |u| match u {
      Unknown::String(s) => Ok(s.clone()),
      Unknown::Null => Err(ParseError::new("", "expected string, got null")),
      _ => Err(ParseError::new("", "expected string")),
    },
  )
}

/// Boolean schema; wire type is [`bool`].
pub fn bool_<E: EffectData + 'static>() -> Schema<bool, bool, E> {
  Schema::make(
    Ok,
    |b| b,
    |u| match u {
      Unknown::Bool(b) => Ok(*b),
      Unknown::Null => Err(ParseError::new("", "expected bool, got null")),
      _ => Err(ParseError::new("", "expected bool")),
    },
  )
}

/// Floating-point schema; wire type is [`f64`].
///
/// [`Schema::decode_unknown`] accepts [`Unknown::F64`] or [`Unknown::I64`] (integer promoted to
/// `f64`) so JSON-shaped configs that use whole numbers still decode.
pub fn f64<E: EffectData + 'static>() -> Schema<f64, f64, E> {
  Schema::make(
    Ok,
    |x| x,
    |u| match u {
      Unknown::F64(x) => Ok(*x),
      Unknown::I64(n) => Ok(*n as f64),
      Unknown::Null => Err(ParseError::new("", "expected f64, got null")),
      _ => Err(ParseError::new("", "expected f64")),
    },
  )
}

/// Map the decoded value (and reverse for encode).
pub fn transform<A, B, I, E, FA, FB>(s: Schema<A, I, E>, decode: FA, encode: FB) -> Schema<B, I, E>
where
  E: EffectData + 'static,
  A: 'static,
  B: 'static,
  I: 'static,
  FA: Fn(A) -> Result<B, ParseError> + Send + Sync + 'static,
  FB: Fn(B) -> A + Send + Sync + 'static,
{
  let decode_f = s.decode.clone();
  let encode_f = s.encode.clone();
  let du = s.decode_unknown.clone();
  let decode = Arc::new(decode);
  let decode2 = decode.clone();
  let encode = Arc::new(encode);
  Schema::make(
    move |i| decode(decode_f(i)?),
    move |b| encode_f(encode(b)),
    move |u| decode2(du(u)?),
  )
}

/// Keep values satisfying `pred`, otherwise fail with `message`.
pub fn filter<A, I, E>(
  s: Schema<A, I, E>,
  pred: impl Fn(&A) -> bool + Send + Sync + 'static,
  message: impl Into<String>,
) -> Schema<A, I, E>
where
  E: EffectData + 'static,
  A: 'static,
  I: 'static,
{
  let pred = Arc::new(pred);
  let decode_f = s.decode.clone();
  let encode_f = s.encode.clone();
  let du = s.decode_unknown.clone();
  let msg: String = message.into();
  let msg2 = msg.clone();
  let p2 = pred.clone();
  Schema::make(
    move |i| {
      let a = decode_f(i)?;
      if pred(&a) {
        Ok(a)
      } else {
        Err(ParseError::new("", msg.clone()))
      }
    },
    move |a| encode_f(a),
    move |u| {
      let a = du(u)?;
      if p2(&a) {
        Ok(a)
      } else {
        Err(ParseError::new("", msg2.clone()))
      }
    },
  )
}

/// Refinement alias for [`filter`].
pub fn refine<A, I, E>(
  s: Schema<A, I, E>,
  pred: impl Fn(&A) -> bool + Send + Sync + 'static,
  message: impl Into<String>,
) -> Schema<A, I, E>
where
  E: EffectData + 'static,
  A: 'static,
  I: 'static,
{
  filter(s, pred, message)
}

/// [`Option`] around inner schema; wire is [`Option`] of the inner wire type.
pub fn optional<A, I, E>(s: Schema<A, I, E>) -> Schema<Option<A>, Option<I>, E>
where
  E: EffectData + 'static,
  A: 'static,
  I: 'static,
{
  let s_dec = s.clone();
  let s_enc = s.clone();
  let s_du = s.clone();
  Schema::make(
    move |oi| match oi {
      None => Ok(None),
      Some(i) => Ok(Some(s_dec.decode(i)?)),
    },
    move |oa| oa.map(|a| s_enc.encode(a)),
    move |u| match u {
      Unknown::Null => Ok(None),
      other => Ok(Some(s_du.decode_unknown(other)?)),
    },
  )
}

/// Homogeneous array / vector.
pub fn array<A, I, E>(s: Schema<A, I, E>) -> Schema<Vec<A>, Vec<I>, E>
where
  E: EffectData + 'static,
  A: 'static,
  I: 'static,
{
  let s_dec = s.clone();
  let s_enc = s.clone();
  let s_du = s.clone();
  Schema::make(
    move |items: Vec<I>| {
      let mut out = Vec::with_capacity(items.len());
      for (idx, item) in items.into_iter().enumerate() {
        match s_dec.decode(item) {
          Ok(a) => out.push(a),
          Err(e) => return Err(e.prefix_index(idx)),
        }
      }
      Ok(out)
    },
    move |values: Vec<A>| values.into_iter().map(|a| s_enc.encode(a)).collect(),
    move |u| match u {
      Unknown::Array(items) => {
        let mut out = Vec::with_capacity(items.len());
        for (idx, item) in items.iter().enumerate() {
          match s_du.decode_unknown(item) {
            Ok(a) => out.push(a),
            Err(e) => return Err(e.prefix_index(idx)),
          }
        }
        Ok(out)
      }
      _ => Err(ParseError::new("", "expected array")),
    },
  )
}

/// Fixed-length tuple (wire = tuple of parts).
pub fn tuple<A0, A1, I0, I1, E>(
  s0: Schema<A0, I0, E>,
  s1: Schema<A1, I1, E>,
) -> Schema<(A0, A1), (I0, I1), E>
where
  E: EffectData + 'static,
  A0: 'static,
  A1: 'static,
  I0: 'static,
  I1: 'static,
{
  let s0_d = s0.clone();
  let s0_e = s0.clone();
  let s0_u = s0.clone();
  let s1_d = s1.clone();
  let s1_e = s1.clone();
  let s1_u = s1.clone();
  Schema::make(
    move |(i0, i1)| {
      let a0 = s0_d.decode(i0).map_err(|e| e.prefix("0"))?;
      let a1 = s1_d.decode(i1).map_err(|e| e.prefix("1"))?;
      Ok((a0, a1))
    },
    move |(a0, a1)| (s0_e.encode(a0), s1_e.encode(a1)),
    move |u| match u {
      Unknown::Array(arr) if arr.len() == 2 => {
        let a0 = s0_u.decode_unknown(&arr[0]).map_err(|e| e.prefix("0"))?;
        let a1 = s1_u.decode_unknown(&arr[1]).map_err(|e| e.prefix("1"))?;
        Ok((a0, a1))
      }
      _ => Err(ParseError::new("", "expected array of length 2")),
    },
  )
}

/// Fixed-length triple (wire = tuple of parts).
pub fn tuple3<A0, A1, A2, I0, I1, I2, E>(
  s0: Schema<A0, I0, E>,
  s1: Schema<A1, I1, E>,
  s2: Schema<A2, I2, E>,
) -> Schema<(A0, A1, A2), (I0, I1, I2), E>
where
  E: EffectData + 'static,
  A0: 'static,
  A1: 'static,
  A2: 'static,
  I0: 'static,
  I1: 'static,
  I2: 'static,
{
  let s0_d = s0.clone();
  let s0_e = s0.clone();
  let s0_u = s0.clone();
  let s1_d = s1.clone();
  let s1_e = s1.clone();
  let s1_u = s1.clone();
  let s2_d = s2.clone();
  let s2_e = s2.clone();
  let s2_u = s2.clone();
  Schema::make(
    move |(i0, i1, i2)| {
      let a0 = s0_d.decode(i0).map_err(|e| e.prefix("0"))?;
      let a1 = s1_d.decode(i1).map_err(|e| e.prefix("1"))?;
      let a2 = s2_d.decode(i2).map_err(|e| e.prefix("2"))?;
      Ok((a0, a1, a2))
    },
    move |(a0, a1, a2)| (s0_e.encode(a0), s1_e.encode(a1), s2_e.encode(a2)),
    move |u| match u {
      Unknown::Array(arr) if arr.len() == 3 => {
        let a0 = s0_u.decode_unknown(&arr[0]).map_err(|e| e.prefix("0"))?;
        let a1 = s1_u.decode_unknown(&arr[1]).map_err(|e| e.prefix("1"))?;
        let a2 = s2_u.decode_unknown(&arr[2]).map_err(|e| e.prefix("2"))?;
        Ok((a0, a1, a2))
      }
      _ => Err(ParseError::new("", "expected array of length 3")),
    },
  )
}

/// Fixed-length quadruple (wire = tuple of parts).
pub fn tuple4<A0, A1, A2, A3, I0, I1, I2, I3, E>(
  s0: Schema<A0, I0, E>,
  s1: Schema<A1, I1, E>,
  s2: Schema<A2, I2, E>,
  s3: Schema<A3, I3, E>,
) -> Schema<(A0, A1, A2, A3), (I0, I1, I2, I3), E>
where
  E: EffectData + 'static,
  A0: 'static,
  A1: 'static,
  A2: 'static,
  A3: 'static,
  I0: 'static,
  I1: 'static,
  I2: 'static,
  I3: 'static,
{
  let s0_d = s0.clone();
  let s0_e = s0.clone();
  let s0_u = s0.clone();
  let s1_d = s1.clone();
  let s1_e = s1.clone();
  let s1_u = s1.clone();
  let s2_d = s2.clone();
  let s2_e = s2.clone();
  let s2_u = s2.clone();
  let s3_d = s3.clone();
  let s3_e = s3.clone();
  let s3_u = s3.clone();
  Schema::make(
    move |(i0, i1, i2, i3)| {
      let a0 = s0_d.decode(i0).map_err(|e| e.prefix("0"))?;
      let a1 = s1_d.decode(i1).map_err(|e| e.prefix("1"))?;
      let a2 = s2_d.decode(i2).map_err(|e| e.prefix("2"))?;
      let a3 = s3_d.decode(i3).map_err(|e| e.prefix("3"))?;
      Ok((a0, a1, a2, a3))
    },
    move |(a0, a1, a2, a3)| {
      (
        s0_e.encode(a0),
        s1_e.encode(a1),
        s2_e.encode(a2),
        s3_e.encode(a3),
      )
    },
    move |u| match u {
      Unknown::Array(arr) if arr.len() == 4 => {
        let a0 = s0_u.decode_unknown(&arr[0]).map_err(|e| e.prefix("0"))?;
        let a1 = s1_u.decode_unknown(&arr[1]).map_err(|e| e.prefix("1"))?;
        let a2 = s2_u.decode_unknown(&arr[2]).map_err(|e| e.prefix("2"))?;
        let a3 = s3_u.decode_unknown(&arr[3]).map_err(|e| e.prefix("3"))?;
        Ok((a0, a1, a2, a3))
      }
      _ => Err(ParseError::new("", "expected array of length 4")),
    },
  )
}

/// Struct with named fields (wire = tuple of fields in declaration order).
pub fn struct_<A0, A1, I0, I1, E>(
  name0: &'static str,
  s0: Schema<A0, I0, E>,
  name1: &'static str,
  s1: Schema<A1, I1, E>,
) -> Schema<(A0, A1), (I0, I1), E>
where
  E: EffectData + 'static,
  A0: 'static,
  A1: 'static,
  I0: 'static,
  I1: 'static,
{
  let s0_d = s0.clone();
  let s0_e = s0.clone();
  let s0_u = s0.clone();
  let s1_d = s1.clone();
  let s1_e = s1.clone();
  let s1_u = s1.clone();
  Schema::make(
    move |(i0, i1)| {
      let a0 = s0_d.decode(i0).map_err(|e| e.prefix(name0))?;
      let a1 = s1_d.decode(i1).map_err(|e| e.prefix(name1))?;
      Ok((a0, a1))
    },
    move |(a0, a1)| (s0_e.encode(a0), s1_e.encode(a1)),
    move |u| {
      let obj = match u {
        Unknown::Object(m) => m,
        _ => return Err(ParseError::new("", "expected object")),
      };
      let u0 = obj
        .get(name0)
        .ok_or_else(|| ParseError::new(name0, format!("missing field {name0}")))?;
      let a0 = s0_u.decode_unknown(u0).map_err(|e| e.prefix(name0))?;
      let u1 = obj
        .get(name1)
        .ok_or_else(|| ParseError::new(name1, format!("missing field {name1}")))?;
      let a1 = s1_u.decode_unknown(u1).map_err(|e| e.prefix(name1))?;
      Ok((a0, a1))
    },
  )
}

/// Struct with three named fields (wire = triple of fields in declaration order).
pub fn struct3<A0, A1, A2, I0, I1, I2, E>(
  name0: &'static str,
  s0: Schema<A0, I0, E>,
  name1: &'static str,
  s1: Schema<A1, I1, E>,
  name2: &'static str,
  s2: Schema<A2, I2, E>,
) -> Schema<(A0, A1, A2), (I0, I1, I2), E>
where
  E: EffectData + 'static,
  A0: 'static,
  A1: 'static,
  A2: 'static,
  I0: 'static,
  I1: 'static,
  I2: 'static,
{
  let s0_d = s0.clone();
  let s0_e = s0.clone();
  let s0_u = s0.clone();
  let s1_d = s1.clone();
  let s1_e = s1.clone();
  let s1_u = s1.clone();
  let s2_d = s2.clone();
  let s2_e = s2.clone();
  let s2_u = s2.clone();
  Schema::make(
    move |(i0, i1, i2)| {
      let a0 = s0_d.decode(i0).map_err(|e| e.prefix(name0))?;
      let a1 = s1_d.decode(i1).map_err(|e| e.prefix(name1))?;
      let a2 = s2_d.decode(i2).map_err(|e| e.prefix(name2))?;
      Ok((a0, a1, a2))
    },
    move |(a0, a1, a2)| (s0_e.encode(a0), s1_e.encode(a1), s2_e.encode(a2)),
    move |u| {
      let obj = match u {
        Unknown::Object(m) => m,
        _ => return Err(ParseError::new("", "expected object")),
      };
      let u0 = obj
        .get(name0)
        .ok_or_else(|| ParseError::new(name0, format!("missing field {name0}")))?;
      let a0 = s0_u.decode_unknown(u0).map_err(|e| e.prefix(name0))?;
      let u1 = obj
        .get(name1)
        .ok_or_else(|| ParseError::new(name1, format!("missing field {name1}")))?;
      let a1 = s1_u.decode_unknown(u1).map_err(|e| e.prefix(name1))?;
      let u2 = obj
        .get(name2)
        .ok_or_else(|| ParseError::new(name2, format!("missing field {name2}")))?;
      let a2 = s2_u.decode_unknown(u2).map_err(|e| e.prefix(name2))?;
      Ok((a0, a1, a2))
    },
  )
}

/// Struct with four named fields (wire = quadruple of fields in declaration order).
#[allow(clippy::too_many_arguments)] // one name + schema per field (4×2 = 8 parameters by design)
pub fn struct4<A0, A1, A2, A3, I0, I1, I2, I3, E>(
  name0: &'static str,
  s0: Schema<A0, I0, E>,
  name1: &'static str,
  s1: Schema<A1, I1, E>,
  name2: &'static str,
  s2: Schema<A2, I2, E>,
  name3: &'static str,
  s3: Schema<A3, I3, E>,
) -> Schema<(A0, A1, A2, A3), (I0, I1, I2, I3), E>
where
  E: EffectData + 'static,
  A0: 'static,
  A1: 'static,
  A2: 'static,
  A3: 'static,
  I0: 'static,
  I1: 'static,
  I2: 'static,
  I3: 'static,
{
  let s0_d = s0.clone();
  let s0_e = s0.clone();
  let s0_u = s0.clone();
  let s1_d = s1.clone();
  let s1_e = s1.clone();
  let s1_u = s1.clone();
  let s2_d = s2.clone();
  let s2_e = s2.clone();
  let s2_u = s2.clone();
  let s3_d = s3.clone();
  let s3_e = s3.clone();
  let s3_u = s3.clone();
  Schema::make(
    move |(i0, i1, i2, i3)| {
      let a0 = s0_d.decode(i0).map_err(|e| e.prefix(name0))?;
      let a1 = s1_d.decode(i1).map_err(|e| e.prefix(name1))?;
      let a2 = s2_d.decode(i2).map_err(|e| e.prefix(name2))?;
      let a3 = s3_d.decode(i3).map_err(|e| e.prefix(name3))?;
      Ok((a0, a1, a2, a3))
    },
    move |(a0, a1, a2, a3)| {
      (
        s0_e.encode(a0),
        s1_e.encode(a1),
        s2_e.encode(a2),
        s3_e.encode(a3),
      )
    },
    move |u| {
      let obj = match u {
        Unknown::Object(m) => m,
        _ => return Err(ParseError::new("", "expected object")),
      };
      let u0 = obj
        .get(name0)
        .ok_or_else(|| ParseError::new(name0, format!("missing field {name0}")))?;
      let a0 = s0_u.decode_unknown(u0).map_err(|e| e.prefix(name0))?;
      let u1 = obj
        .get(name1)
        .ok_or_else(|| ParseError::new(name1, format!("missing field {name1}")))?;
      let a1 = s1_u.decode_unknown(u1).map_err(|e| e.prefix(name1))?;
      let u2 = obj
        .get(name2)
        .ok_or_else(|| ParseError::new(name2, format!("missing field {name2}")))?;
      let a2 = s2_u.decode_unknown(u2).map_err(|e| e.prefix(name2))?;
      let u3 = obj
        .get(name3)
        .ok_or_else(|| ParseError::new(name3, format!("missing field {name3}")))?;
      let a3 = s3_u.decode_unknown(u3).map_err(|e| e.prefix(name3))?;
      Ok((a0, a1, a2, a3))
    },
  )
}

/// Try `primary` first, then `fallback` (both share semantic type `A`; wire is [`Unknown`]).
pub fn union_<A, E>(
  primary: Schema<A, Unknown, E>,
  fallback: Schema<A, Unknown, E>,
) -> Schema<A, Unknown, E>
where
  E: EffectData + 'static,
  A: 'static,
{
  let primary = primary.clone();
  let fallback_dec = fallback.clone();
  let fallback_du = fallback.clone();
  let primary_enc = primary.clone();
  let primary_du = primary.clone();
  Schema::make(
    move |u: Unknown| {
      primary
        .decode(u.clone())
        .or_else(|_| fallback_dec.decode(u))
    },
    move |a| primary_enc.encode(a),
    move |u| {
      primary_du
        .decode_unknown(u)
        .or_else(|_| fallback_du.decode_unknown(u))
    },
  )
}

#[cfg(test)]
mod tests {
  use super::*;
  use rstest::rstest;

  fn person_schema() -> Schema<(String, i64), (String, i64), ()> {
    struct_("name", string(), "age", i64())
  }

  mod struct_schema_two_fields {
    use super::*;

    #[test]
    fn decode_when_wire_matches_returns_decoded_tuple() {
      let s = person_schema();
      let wire = ("alice".to_string(), 30_i64);
      let got = s.decode(wire).expect("decode");
      assert_eq!(got.0, "alice");
      assert_eq!(got.1, 30);
      assert_eq!(s.encode(got), ("alice".to_string(), 30_i64));
    }

    #[test]
    fn decode_unknown_success_returns_tuple() {
      let s = person_schema();
      let mut m = BTreeMap::new();
      m.insert("name".into(), Unknown::String("bob".into()));
      m.insert("age".into(), Unknown::I64(25));
      let got = s.decode_unknown(&Unknown::Object(m)).expect("ok");
      assert_eq!(got, ("bob".to_string(), 25_i64));
    }

    #[test]
    fn decode_unknown_non_object_fails() {
      let s = person_schema();
      assert!(s.decode_unknown(&Unknown::I64(1)).is_err());
    }

    #[test]
    fn decode_unknown_missing_second_field_fails() {
      let s = person_schema();
      let mut m = BTreeMap::new();
      m.insert("name".into(), Unknown::String("alice".into()));
      // "age" is missing
      let err = s
        .decode_unknown(&Unknown::Object(m))
        .expect_err("missing age");
      assert!(err.path.contains("age"), "path was {:?}", err.path);
    }

    #[test]
    fn decode_unknown_when_nested_field_wrong_type_prefixes_path() {
      let inner = struct_("street", string::<()>(), "zip", string::<()>());
      let s = struct_("user", inner, "id", i64::<()>());

      let mut user = BTreeMap::new();
      let mut addr = BTreeMap::new();
      addr.insert("street".into(), Unknown::String("Main".into()));
      addr.insert("zip".into(), Unknown::I64(12345)); // wrong type
      user.insert("user".into(), Unknown::Object(addr));
      user.insert("id".into(), Unknown::I64(1));
      let root = Unknown::Object(user);

      let err = s.decode_unknown(&root).expect_err("zip should fail");
      assert!(
        err.path.contains("user") && err.path.contains("zip"),
        "path {:?} should mention user and zip",
        err.path
      );
    }
  }

  mod bool_codec {
    use super::*;

    #[test]
    fn decode_when_wire_bool_round_trips() {
      let s = bool_::<()>();
      assert_eq!(s.decode(true), Ok(true));
      assert_eq!(s.encode(false), false);
    }

    #[rstest]
    #[case(Unknown::Bool(true), true)]
    #[case(Unknown::Bool(false), false)]
    fn decode_unknown_when_bool_returns_value(#[case] input: Unknown, #[case] want: bool) {
      assert_eq!(bool_::<()>().decode_unknown(&input).unwrap(), want);
    }

    #[test]
    fn decode_unknown_when_null_fails() {
      assert!(bool_::<()>().decode_unknown(&Unknown::Null).is_err());
    }

    #[test]
    fn decode_unknown_when_i64_fails() {
      assert!(bool_::<()>().decode_unknown(&Unknown::I64(0)).is_err());
    }
  }

  mod f64_codec {
    use super::*;

    #[test]
    fn decode_when_wire_f64_round_trips() {
      let s = f64::<()>();
      assert_eq!(s.decode(1.5_f64), Ok(1.5_f64));
      assert_eq!(s.encode(-0.0_f64), -0.0_f64);
    }

    #[rstest]
    #[case(Unknown::F64(2.25), 2.25_f64)]
    #[case(Unknown::I64(7), 7.0_f64)]
    fn decode_unknown_when_numeric_returns_f64(#[case] input: Unknown, #[case] want: f64) {
      assert_eq!(f64::<()>().decode_unknown(&input).unwrap(), want);
    }

    #[test]
    fn decode_unknown_when_null_fails() {
      assert!(f64::<()>().decode_unknown(&Unknown::Null).is_err());
    }

    #[test]
    fn decode_unknown_when_string_fails() {
      assert!(
        f64::<()>()
          .decode_unknown(&Unknown::String("x".into()))
          .is_err()
      );
    }
  }

  mod tuple3_codec {
    use super::*;

    #[test]
    fn decode_when_three_wires_match_returns_triple() {
      let s = tuple3(i64::<()>(), string::<()>(), bool_::<()>());
      let wire = (1_i64, "x".to_string(), true);
      let got = s.decode(wire.clone()).expect("decode");
      assert_eq!(got, (1_i64, "x".to_string(), true));
      assert_eq!(s.encode(got), wire);
    }

    #[test]
    fn decode_unknown_when_array_length_three_succeeds() {
      let s = tuple3(i64::<()>(), string::<()>(), bool_::<()>());
      let u = Unknown::Array(vec![
        Unknown::I64(9),
        Unknown::String("z".into()),
        Unknown::Bool(false),
      ]);
      assert_eq!(
        s.decode_unknown(&u).expect("ok"),
        (9_i64, "z".to_string(), false)
      );
    }

    #[test]
    fn decode_unknown_when_array_wrong_length_fails() {
      let s = tuple3(i64::<()>(), string::<()>(), bool_::<()>());
      let u = Unknown::Array(vec![Unknown::I64(1), Unknown::I64(2)]);
      assert!(s.decode_unknown(&u).is_err());
    }
  }

  mod tuple4_codec {
    use super::*;

    #[test]
    fn decode_unknown_when_array_length_four_succeeds() {
      let s = tuple4(i64::<()>(), bool_::<()>(), string::<()>(), f64::<()>());
      let u = Unknown::Array(vec![
        Unknown::I64(1),
        Unknown::Bool(true),
        Unknown::String("z".into()),
        Unknown::F64(0.5),
      ]);
      let got = s.decode_unknown(&u).expect("ok");
      assert_eq!(got, (1_i64, true, "z".to_string(), 0.5_f64));
    }
  }

  mod struct4_codec {
    use super::*;

    #[test]
    fn decode_unknown_when_four_fields_present_succeeds() {
      let s = struct4(
        "w",
        i64::<()>(),
        "x",
        bool_::<()>(),
        "y",
        string::<()>(),
        "z",
        f64::<()>(),
      );
      let mut m = BTreeMap::new();
      m.insert("w".into(), Unknown::I64(0));
      m.insert("x".into(), Unknown::Bool(false));
      m.insert("y".into(), Unknown::String("q".into()));
      m.insert("z".into(), Unknown::F64(2.0));
      let got = s.decode_unknown(&Unknown::Object(m)).expect("decode");
      assert_eq!(got, (0_i64, false, "q".to_string(), 2.0_f64));
    }
  }

  mod struct3_codec {
    use super::*;

    #[test]
    fn decode_unknown_when_three_fields_present_succeeds() {
      let s = struct3("a", i64::<()>(), "b", string::<()>(), "c", bool_::<()>());
      let mut m = BTreeMap::new();
      m.insert("a".into(), Unknown::I64(1));
      m.insert("b".into(), Unknown::String("hi".into()));
      m.insert("c".into(), Unknown::Bool(true));
      let got = s.decode_unknown(&Unknown::Object(m)).expect("decode");
      assert_eq!(got, (1_i64, "hi".to_string(), true));
    }

    #[test]
    fn decode_unknown_when_middle_field_missing_reports_path() {
      let s = struct3("x", i64::<()>(), "y", string::<()>(), "z", i64::<()>());
      let mut m = BTreeMap::new();
      m.insert("x".into(), Unknown::I64(0));
      m.insert("z".into(), Unknown::I64(3));
      let err = s
        .decode_unknown(&Unknown::Object(m))
        .expect_err("missing y");
      assert!(err.path.contains("y"), "path was {:?}", err.path);
    }
  }

  mod optional_codec {
    use super::*;

    #[test]
    fn decode_unknown_when_null_returns_none() {
      let s = optional(i64::<()>());
      assert_eq!(s.decode_unknown(&Unknown::Null).expect("null"), None);
    }

    #[test]
    fn decode_when_option_none_and_some_round_trip() {
      let s = optional(i64::<()>());
      assert!(s.decode(None).expect("none").is_none());
      assert_eq!(s.decode(Some(7_i64)).expect("some").unwrap(), 7_i64);
    }
  }

  mod refine_codec {
    use super::*;

    #[test]
    fn decode_rejects_when_predicate_fails() {
      let s = refine(i64::<()>(), |n| *n >= 10, "must be at least 10");
      assert_eq!(s.decode(12).expect("ok"), 12);
      let err = s.decode(5).expect_err("below threshold");
      assert!(err.message.contains("10"));
    }
  }

  mod more_combinators {
    use super::*;

    #[test]
    fn i64_unknown_wire_round_trips_through_unknown() {
      let s = i64_unknown_wire::<()>();
      assert_eq!(s.decode_unknown(&Unknown::I64(7)).unwrap(), 7);
      assert_eq!(s.encode(7), Unknown::I64(7));
    }

    #[test]
    fn array_decodes_unknown_vector() {
      let s = array(i64::<()>());
      let u = Unknown::Array(vec![Unknown::I64(1), Unknown::I64(2)]);
      assert_eq!(s.decode_unknown(&u).unwrap(), vec![1, 2]);
      let wire = vec![3_i64, 4_i64];
      assert_eq!(s.decode(wire.clone()).unwrap(), vec![3, 4]);
      assert_eq!(s.encode(vec![3, 4]), wire);
    }

    #[test]
    fn tuple2_decodes_unknown_array() {
      let s = tuple(i64::<()>(), bool_::<()>());
      let u = Unknown::Array(vec![Unknown::I64(1), Unknown::Bool(true)]);
      assert_eq!(s.decode_unknown(&u).unwrap(), (1, true));
    }

    #[test]
    fn filter_decode_unknown_checks_predicate() {
      let s = filter(i64::<()>(), |n| *n == 7, "seven");
      assert_eq!(s.decode_unknown(&Unknown::I64(7)).unwrap(), 7);
      assert_eq!(s.decode_unknown(&Unknown::I64(8)).is_err(), true);
    }

    #[test]
    fn filter_encode_returns_value() {
      let s = filter(i64::<()>(), |n| *n > 0, "positive");
      assert_eq!(s.encode(5), 5);
    }

    #[test]
    fn transform_encode_path() {
      let s = transform(
        i64::<()>(),
        |n| Ok(n.to_string()),
        |t: String| t.parse::<i64>().unwrap(),
      );
      assert_eq!(s.encode("42".to_string()), 42_i64);
    }

    #[test]
    fn transform_maps_decode_and_unknown() {
      let s = transform(
        i64::<()>(),
        |n| Ok(n.to_string()),
        |t| t.parse::<i64>().unwrap(),
      );
      assert_eq!(s.decode(5_i64).unwrap(), "5");
      assert_eq!(s.decode_unknown(&Unknown::I64(9)).unwrap(), "9");
    }

    #[test]
    fn union_uses_fallback_when_primary_rejects() {
      let primary = filter(i64_unknown_wire::<()>(), |n| *n < 0, "negative only");
      let fallback = i64_unknown_wire::<()>();
      let s = union_(primary, fallback);
      assert_eq!(s.decode_unknown(&Unknown::I64(-1)).unwrap(), -1);
      assert_eq!(s.decode_unknown(&Unknown::I64(5)).unwrap(), 5);
    }
  }

  mod parse_error_tests {
    use super::*;

    #[test]
    fn prefix_index_prepends_numeric_index() {
      let e = ParseError::new("field", "bad value");
      let prefixed = e.prefix_index(3);
      assert_eq!(prefixed.path, "3.field");
    }

    #[test]
    fn prefix_index_on_empty_path() {
      let e = ParseError::new("", "bad value");
      let prefixed = e.prefix_index(0);
      assert_eq!(prefixed.path, "0");
    }
  }

  mod i64_codec_extra {
    use super::*;

    #[test]
    fn i64_decode_unknown_non_null_non_i64_fails() {
      assert!(
        i64::<()>()
          .decode_unknown(&Unknown::String("x".into()))
          .is_err()
      );
    }

    #[test]
    fn i64_unknown_wire_decode_wire_success() {
      let s = i64_unknown_wire::<()>();
      assert_eq!(s.decode(Unknown::I64(7)).unwrap(), 7);
    }

    #[test]
    fn i64_unknown_wire_decode_wire_null_fails() {
      let s = i64_unknown_wire::<()>();
      assert!(s.decode(Unknown::Null).is_err());
    }

    #[test]
    fn i64_unknown_wire_decode_wire_non_i64_fails() {
      let s = i64_unknown_wire::<()>();
      assert!(s.decode(Unknown::String("x".into())).is_err());
    }

    #[test]
    fn i64_unknown_wire_decode_unknown_null_fails() {
      let s = i64_unknown_wire::<()>();
      assert!(s.decode_unknown(&Unknown::Null).is_err());
    }

    #[test]
    fn i64_unknown_wire_decode_unknown_non_i64_fails() {
      let s = i64_unknown_wire::<()>();
      assert!(s.decode_unknown(&Unknown::String("x".into())).is_err());
    }
  }

  mod string_codec_extra {
    use super::*;

    #[test]
    fn string_decode_unknown_non_null_non_string_fails() {
      assert!(string::<()>().decode_unknown(&Unknown::I64(1)).is_err());
    }
  }

  mod optional_codec_extra {
    use super::*;

    #[test]
    fn optional_encode_some() {
      let s = optional(i64::<()>());
      assert_eq!(s.encode(Some(5_i64)), Some(5_i64));
    }

    #[test]
    fn optional_encode_none() {
      let s = optional(i64::<()>());
      assert_eq!(s.encode(None), None);
    }

    #[test]
    fn optional_decode_unknown_non_null_returns_some() {
      let s = optional(i64::<()>());
      assert_eq!(s.decode_unknown(&Unknown::I64(42)).unwrap(), Some(42_i64));
    }
  }

  mod array_codec_extra {
    use super::*;

    #[test]
    fn array_decode_error_prefixes_index() {
      let s = array(filter(i64::<()>(), |n| *n > 0, "positive"));
      let wire = vec![1_i64, -1_i64, 3_i64];
      let err = s.decode(wire).expect_err("negative should fail");
      assert!(err.path.contains("1"), "path was {:?}", err.path);
    }

    #[test]
    fn array_decode_unknown_non_array_fails() {
      let s = array(i64::<()>());
      assert!(s.decode_unknown(&Unknown::I64(1)).is_err());
    }

    #[test]
    fn array_decode_unknown_success() {
      let s = array(i64::<()>());
      let u = Unknown::Array(vec![Unknown::I64(10), Unknown::I64(20)]);
      assert_eq!(s.decode_unknown(&u).unwrap(), vec![10_i64, 20_i64]);
    }
  }

  mod tuple_codec_extra {
    use super::*;

    #[test]
    fn tuple_decode_wire_success() {
      let s = tuple(i64::<()>(), string::<()>());
      assert_eq!(
        s.decode((5_i64, "hi".to_string())).unwrap(),
        (5_i64, "hi".to_string())
      );
    }

    #[test]
    fn tuple_encode_wire() {
      let s = tuple(i64::<()>(), string::<()>());
      assert_eq!(s.encode((3_i64, "x".to_string())), (3_i64, "x".to_string()));
    }

    #[test]
    fn tuple_decode_unknown_non_array_of_2_fails() {
      let s = tuple(i64::<()>(), bool_::<()>());
      assert!(s.decode_unknown(&Unknown::I64(1)).is_err());
    }
  }

  mod tuple4_codec_extra {
    use super::*;

    #[test]
    fn tuple4_decode_wire_success() {
      let s = tuple4(i64::<()>(), bool_::<()>(), string::<()>(), f64::<()>());
      let got = s.decode((1_i64, true, "z".to_string(), 2.5_f64)).unwrap();
      assert_eq!(got, (1_i64, true, "z".to_string(), 2.5_f64));
    }

    #[test]
    fn tuple4_encode_wire() {
      let s = tuple4(i64::<()>(), bool_::<()>(), string::<()>(), f64::<()>());
      let encoded = s.encode((2_i64, false, "q".to_string(), 0.5_f64));
      assert_eq!(encoded, (2_i64, false, "q".to_string(), 0.5_f64));
    }

    #[test]
    fn tuple4_decode_unknown_wrong_length_fails() {
      let s = tuple4(i64::<()>(), bool_::<()>(), string::<()>(), f64::<()>());
      let u = Unknown::Array(vec![Unknown::I64(1), Unknown::Bool(true)]);
      assert!(s.decode_unknown(&u).is_err());
    }
  }

  mod struct3_codec_extra {
    use super::*;

    #[test]
    fn struct3_decode_wire_success() {
      let s = struct3("a", i64::<()>(), "b", string::<()>(), "c", bool_::<()>());
      let got = s.decode((1_i64, "x".to_string(), true)).unwrap();
      assert_eq!(got, (1_i64, "x".to_string(), true));
    }

    #[test]
    fn struct3_decode_wire_encode_round_trip() {
      let s = struct3("x", i64::<()>(), "y", i64::<()>(), "z", i64::<()>());
      let wire = (1_i64, 2_i64, 3_i64);
      let got = s.decode(wire).unwrap();
      assert_eq!(s.encode(got), (1_i64, 2_i64, 3_i64));
    }
  }

  mod struct4_codec_extra {
    use super::*;

    #[test]
    fn struct4_decode_wire_success() {
      let s = struct4(
        "a",
        i64::<()>(),
        "b",
        bool_::<()>(),
        "c",
        string::<()>(),
        "d",
        f64::<()>(),
      );
      let got = s.decode((1_i64, true, "x".to_string(), 2.5_f64)).unwrap();
      assert_eq!(got, (1_i64, true, "x".to_string(), 2.5_f64));
    }

    #[test]
    fn struct4_encode_round_trip() {
      let s = struct4(
        "a",
        i64::<()>(),
        "b",
        i64::<()>(),
        "c",
        i64::<()>(),
        "d",
        i64::<()>(),
      );
      let val = (1_i64, 2_i64, 3_i64, 4_i64);
      let encoded = s.encode(val);
      assert_eq!(encoded, (1_i64, 2_i64, 3_i64, 4_i64));
    }

    #[test]
    fn struct4_decode_unknown_missing_field_errors() {
      let s = struct4(
        "a",
        i64::<()>(),
        "b",
        i64::<()>(),
        "c",
        i64::<()>(),
        "d",
        i64::<()>(),
      );
      let mut m = BTreeMap::new();
      m.insert("a".into(), Unknown::I64(1));
      m.insert("b".into(), Unknown::I64(2));
      // "c" and "d" missing
      assert!(s.decode_unknown(&Unknown::Object(m)).is_err());
    }
  }

  mod union_codec_extra {
    use super::*;

    #[test]
    fn union_decode_wire_uses_primary_first() {
      let primary = filter(i64_unknown_wire::<()>(), |n| *n > 0, "positive");
      let fallback = i64_unknown_wire::<()>();
      let s = union_(primary, fallback);
      assert_eq!(s.decode(Unknown::I64(5)).unwrap(), 5);
    }

    #[test]
    fn union_decode_wire_falls_back_when_primary_fails() {
      let primary = filter(i64_unknown_wire::<()>(), |n| *n < 0, "negative");
      let fallback = i64_unknown_wire::<()>();
      let s = union_(primary, fallback);
      assert_eq!(s.decode(Unknown::I64(5)).unwrap(), 5);
    }

    #[test]
    fn union_encode_uses_primary_encoder() {
      let primary = filter(i64_unknown_wire::<()>(), |n| *n > 0, "positive");
      let fallback = i64_unknown_wire::<()>();
      let s = union_(primary, fallback);
      assert_eq!(s.encode(42_i64), Unknown::I64(42));
    }
  }

  mod refine_tests {
    use super::*;

    #[test]
    fn refine_accepts_valid_value() {
      let s = refine(i64::<()>(), |n| *n >= 0, "non-negative");
      assert_eq!(s.decode(5_i64).unwrap(), 5);
    }

    #[test]
    fn refine_rejects_invalid_value() {
      let s = refine(i64::<()>(), |n| *n >= 0, "non-negative");
      assert!(s.decode(-1_i64).is_err());
    }

    #[test]
    fn refine_encode_passes_through() {
      let s = refine(i64::<()>(), |n| *n >= 0, "non-negative");
      assert_eq!(s.encode(7_i64), 7_i64);
    }
  }

  mod parse_error_display {
    use super::*;

    #[test]
    fn parse_error_new_fields_accessible() {
      let e = ParseError::new("user.name", "too short");
      assert_eq!(e.path, "user.name");
      assert_eq!(e.message, "too short");
    }

    #[test]
    fn parse_error_prefix_prepends_segment() {
      let e = ParseError::new("age", "invalid");
      let prefixed = e.prefix("user");
      assert_eq!(prefixed.path, "user.age");
    }

    #[test]
    fn parse_error_prefix_empty_path_becomes_segment() {
      let e = ParseError::new("", "invalid");
      let prefixed = e.prefix("name");
      assert_eq!(prefixed.path, "name");
    }
  }

  mod unknown_display_tests {
    use super::*;

    #[test]
    fn unknown_null_debug() {
      let _ = format!("{:?}", Unknown::Null);
    }

    #[test]
    fn unknown_f64_variant_accessible() {
      #[allow(clippy::approx_constant)]
      let u = Unknown::F64(3.14);
      if let Unknown::F64(v) = u {
        #[allow(clippy::approx_constant)]
        let expected = 3.14_f64;
        assert!((v - expected).abs() < 1e-9);
      } else {
        panic!("not F64");
      }
    }

    #[test]
    fn unknown_array_and_object_roundtrip_debug() {
      let arr = Unknown::Array(vec![Unknown::Bool(true), Unknown::I64(1)]);
      let _ = format!("{:?}", arr);
      let mut m = BTreeMap::new();
      m.insert("k".to_string(), Unknown::String("v".to_string()));
      let obj = Unknown::Object(m);
      let _ = format!("{:?}", obj);
    }
  }
}
