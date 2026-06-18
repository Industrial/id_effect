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
  fn person_schema() -> Schema<(String, i64), (String, i64), ()> {
    struct_("name", string(), "age", i64())
  }

  mod more_combinators {
    use super::*;

    #[test]
    fn wide_codec_combinator_coverage() {
      let s3 = struct3("a", i64::<()>(), "b", string::<()>(), "c", bool_::<()>());
      let mut m = BTreeMap::new();
      m.insert("a".into(), Unknown::I64(1));
      m.insert("c".into(), Unknown::Bool(true));
      assert!(s3.decode_unknown(&Unknown::Object(m)).is_err());

      let mut ok3 = BTreeMap::new();
      ok3.insert("a".into(), Unknown::I64(1));
      ok3.insert("b".into(), Unknown::String("x".into()));
      ok3.insert("c".into(), Unknown::Bool(true));
      assert_eq!(
        s3.decode_unknown(&Unknown::Object(ok3)).unwrap(),
        (1_i64, "x".to_string(), true)
      );

      let arr = tuple4(i64::<()>(), bool_::<()>(), string::<()>(), f64::<()>());
      assert!(
        arr
          .decode_unknown(&Unknown::Array(vec![Unknown::I64(1)]))
          .is_err()
      );
      let arr_ok = Unknown::Array(vec![
        Unknown::I64(1),
        Unknown::Bool(true),
        Unknown::String("z".into()),
        Unknown::F64(2.5),
      ]);
      assert_eq!(
        arr.decode_unknown(&arr_ok).unwrap(),
        (1_i64, true, "z".to_string(), 2.5_f64)
      );

      let filtered = filter(i64::<()>(), |n| *n > 0, "positive");
      assert!(filtered.decode_unknown(&Unknown::I64(-1)).is_err());
      assert_eq!(filtered.decode(7_i64).unwrap(), 7);

      let tr = transform(i64::<()>(), |n| Ok(n.to_string()), |t| t.parse().unwrap());
      assert_eq!(tr.decode(5_i64).unwrap(), "5");
      assert_eq!(tr.encode("9".to_string()), 9_i64);
      assert!(tr.decode_unknown(&Unknown::String("x".into())).is_err());

      let opt = optional(i64::<()>());
      assert_eq!(opt.decode_unknown(&Unknown::Null).unwrap(), None);
      assert_eq!(opt.decode_unknown(&Unknown::I64(9)).unwrap(), Some(9));
      assert_eq!(opt.decode(None).unwrap(), None);
      assert_eq!(opt.decode(Some(3_i64)).unwrap(), Some(3));

      let primary = filter(i64_unknown_wire::<()>(), |n| *n < 0, "negative only");
      let fallback = i64_unknown_wire::<()>();
      let s = union_(primary, fallback);
      assert_eq!(s.decode_unknown(&Unknown::I64(-1)).unwrap(), -1);
      assert_eq!(s.decode_unknown(&Unknown::I64(5)).unwrap(), 5);
      assert_eq!(s.decode(Unknown::I64(-2)).unwrap(), -2);
      assert_eq!(s.encode(8_i64), Unknown::I64(8));

      let s4 = struct4(
        "a",
        i64::<()>(),
        "b",
        bool_::<()>(),
        "c",
        string::<()>(),
        "d",
        f64::<()>(),
      );
      let mut s4m = BTreeMap::new();
      s4m.insert("a".into(), Unknown::I64(1));
      s4m.insert("b".into(), Unknown::Bool(true));
      s4m.insert("c".into(), Unknown::String("w".into()));
      s4m.insert("d".into(), Unknown::F64(0.25));
      assert_eq!(
        s4.decode_unknown(&Unknown::Object(s4m)).unwrap(),
        (1_i64, true, "w".to_string(), 0.25_f64)
      );

      let t3 = tuple3(i64::<()>(), string::<()>(), bool_::<()>());
      let t3u = Unknown::Array(vec![
        Unknown::I64(2),
        Unknown::String("y".into()),
        Unknown::Bool(false),
      ]);
      assert_eq!(
        t3.decode_unknown(&t3u).unwrap(),
        (2_i64, "y".to_string(), false)
      );

      assert_eq!(string::<()>().decode("hi".to_string()).unwrap(), "hi");
      assert_eq!(string::<()>().encode("hi".to_string()), "hi");
      assert_eq!(
        string::<()>()
          .decode_unknown(&Unknown::String("wire".into()))
          .unwrap(),
        "wire"
      );
      assert!(string::<()>().decode_unknown(&Unknown::I64(1)).is_err());

      assert_eq!(bool_::<()>().decode(true).unwrap(), true);
      assert_eq!(bool_::<()>().encode(true), true);
      assert_eq!(f64::<()>().decode(1.5_f64).unwrap(), 1.5);
      assert_eq!(f64::<()>().encode(2.25_f64), 2.25_f64);

      let bad_arr = array(i64::<()>());
      let bad_u = Unknown::Array(vec![Unknown::String("nope".into())]);
      assert!(bad_arr.decode_unknown(&bad_u).is_err());

      let person = struct_("name", string::<()>(), "age", i64::<()>());
      let wire = ("bob".to_string(), 21_i64);
      assert_eq!(person.decode(wire.clone()).unwrap(), wire.clone());
      assert_eq!(person.encode(wire.clone()), wire);
      let mut pm = BTreeMap::new();
      pm.insert("name".into(), Unknown::String("bob".into()));
      pm.insert("age".into(), Unknown::I64(21));
      assert_eq!(
        person.decode_unknown(&Unknown::Object(pm)).unwrap(),
        ("bob".to_string(), 21_i64)
      );

      let t2 = tuple(i64::<()>(), string::<()>());
      let t2u = Unknown::Array(vec![Unknown::I64(4), Unknown::String("t".into())]);
      assert_eq!(t2.decode_unknown(&t2u).unwrap(), (4_i64, "t".to_string()));
      assert_eq!(
        t2.decode((5_i64, "u".to_string())).unwrap(),
        (5_i64, "u".to_string())
      );
      assert_eq!(
        t2.encode((6_i64, "v".to_string())),
        (6_i64, "v".to_string())
      );

      let t3w = tuple3(i64::<()>(), string::<()>(), bool_::<()>());
      assert_eq!(
        t3w.decode((1_i64, "a".to_string(), true)).unwrap(),
        (1_i64, "a".to_string(), true)
      );

      let s4w = struct4(
        "a",
        i64::<()>(),
        "b",
        i64::<()>(),
        "c",
        i64::<()>(),
        "d",
        i64::<()>(),
      );
      let s4wire = (1_i64, 2_i64, 3_i64, 4_i64);
      assert_eq!(s4w.decode(s4wire).unwrap(), s4wire);
      assert_eq!(s4w.encode(s4wire), s4wire);

      let arr_s = array(string::<()>());
      assert_eq!(
        arr_s
          .decode(vec!["a".to_string(), "b".to_string()])
          .unwrap(),
        vec!["a".to_string(), "b".to_string()]
      );
      assert_eq!(arr_s.encode(vec!["c".to_string()]), vec!["c".to_string()]);

      let neg = refine(i64::<()>(), |n| *n < 0, "negative");
      assert_eq!(neg.decode(-3_i64).unwrap(), -3);
      assert_eq!(neg.encode(-3_i64), -3_i64);

      assert!(
        i64::<()>()
          .decode_unknown(&Unknown::String("x".into()))
          .is_err()
      );
      let iw = i64_unknown_wire::<()>();
      assert_eq!(iw.decode(Unknown::I64(7)).unwrap(), 7);
      assert!(iw.decode(Unknown::Null).is_err());
      assert!(iw.decode(Unknown::String("x".into())).is_err());
      assert!(iw.decode_unknown(&Unknown::Null).is_err());
      assert!(iw.decode_unknown(&Unknown::String("x".into())).is_err());

      let opti = optional(i64::<()>());
      assert_eq!(opti.encode(Some(5_i64)), Some(5_i64));
      assert_eq!(opti.encode(None), None);
      assert_eq!(
        opti.decode_unknown(&Unknown::I64(42)).unwrap(),
        Some(42_i64)
      );

      let arr_i = array(i64::<()>());
      assert!(arr_i.decode_unknown(&Unknown::I64(1)).is_err());
      let uarr = Unknown::Array(vec![Unknown::I64(10), Unknown::I64(20)]);
      assert_eq!(arr_i.decode_unknown(&uarr).unwrap(), vec![10_i64, 20_i64]);
      let arr_f = array(filter(i64::<()>(), |n| *n > 0, "positive"));
      let err = arr_f
        .decode(vec![1_i64, -1_i64, 3_i64])
        .expect_err("negative should fail");
      assert!(err.path.contains('1'), "path was {:?}", err.path);

      let t_wire = tuple(i64::<()>(), string::<()>());
      assert_eq!(
        t_wire.decode((5_i64, "hi".to_string())).unwrap(),
        (5_i64, "hi".to_string())
      );
      assert_eq!(
        t_wire.encode((3_i64, "x".to_string())),
        (3_i64, "x".to_string())
      );
      assert!(
        tuple(i64::<()>(), bool_::<()>())
          .decode_unknown(&Unknown::I64(1))
          .is_err()
      );

      let t4 = tuple4(i64::<()>(), bool_::<()>(), string::<()>(), f64::<()>());
      assert_eq!(
        t4.decode((1_i64, true, "z".to_string(), 2.5_f64)).unwrap(),
        (1_i64, true, "z".to_string(), 2.5_f64)
      );
      assert_eq!(
        t4.encode((2_i64, false, "q".to_string(), 0.5_f64)),
        (2_i64, false, "q".to_string(), 0.5_f64)
      );
      let short = Unknown::Array(vec![Unknown::I64(1), Unknown::Bool(true)]);
      assert!(t4.decode_unknown(&short).is_err());

      let s3w = struct3("a", i64::<()>(), "b", string::<()>(), "c", bool_::<()>());
      assert_eq!(
        s3w.decode((1_i64, "x".to_string(), true)).unwrap(),
        (1_i64, "x".to_string(), true)
      );
      let s3r = struct3("x", i64::<()>(), "y", i64::<()>(), "z", i64::<()>());
      let triple = (1_i64, 2_i64, 3_i64);
      assert_eq!(s3r.decode(triple).unwrap(), triple);
      assert_eq!(s3r.encode(triple), triple);

      let s4miss = struct4(
        "a",
        i64::<()>(),
        "b",
        i64::<()>(),
        "c",
        i64::<()>(),
        "d",
        i64::<()>(),
      );
      let mut partial = BTreeMap::new();
      partial.insert("a".into(), Unknown::I64(1));
      partial.insert("b".into(), Unknown::I64(2));
      assert!(s4miss.decode_unknown(&Unknown::Object(partial)).is_err());

      let pos = filter(i64_unknown_wire::<()>(), |n| *n > 0, "positive");
      let neg_only = filter(i64_unknown_wire::<()>(), |n| *n < 0, "negative");
      let u_pos = union_(pos, i64_unknown_wire::<()>());
      assert_eq!(u_pos.decode(Unknown::I64(5)).unwrap(), 5);
      let u_neg = union_(neg_only, i64_unknown_wire::<()>());
      assert_eq!(u_neg.decode(Unknown::I64(5)).unwrap(), 5);
      assert_eq!(u_pos.encode(42_i64), Unknown::I64(42));

      let nn = refine(i64::<()>(), |n| *n >= 0, "non-negative");
      assert_eq!(nn.decode(5_i64).unwrap(), 5);
      assert!(nn.decode(-1_i64).is_err());
      assert_eq!(nn.encode(7_i64), 7_i64);

      let cloned = s.clone();
      assert_eq!(cloned.decode_unknown(&Unknown::I64(5)).unwrap(), 5);

      let custom = Schema::<i32, i32, ()>::make(
        |x| Ok(x),
        |x| x,
        |u| match u {
          Unknown::I64(n) => Ok(*n as i32),
          _ => Err(ParseError::new("", "bad")),
        },
      );
      assert_eq!(custom.decode(9).unwrap(), 9);
      assert_eq!(custom.encode(9), 9);
      assert_eq!(custom.decode_unknown(&Unknown::I64(9)).unwrap(), 9);

      let e = ParseError::new("user.name", "too short");
      assert_eq!(e.path, "user.name");
      let prefixed = e.prefix("form");
      assert_eq!(prefixed.path, "form.user.name");
      let _ = format!("{:?}", Unknown::Null);
      #[allow(clippy::approx_constant)]
      let _ = format!("{:?}", Unknown::F64(3.14));

      let ps = person_schema();
      let wire = ("alice".to_string(), 30_i64);
      assert_eq!(ps.decode(wire.clone()).unwrap(), wire);
      let mut pm = BTreeMap::new();
      pm.insert("name".into(), Unknown::String("bob".into()));
      pm.insert("age".into(), Unknown::I64(25));
      assert_eq!(
        ps.decode_unknown(&Unknown::Object(pm)).unwrap(),
        ("bob".to_string(), 25_i64)
      );
      assert!(ps.decode_unknown(&Unknown::I64(1)).is_err());
      let mut pmiss = BTreeMap::new();
      pmiss.insert("name".into(), Unknown::String("alice".into()));
      let err_age = ps
        .decode_unknown(&Unknown::Object(pmiss))
        .expect_err("missing age");
      assert!(err_age.path.contains("age"));

      let inner = struct_("street", string::<()>(), "zip", string::<()>());
      let nested = struct_("user", inner, "id", i64::<()>());
      let mut user = BTreeMap::new();
      let mut addr = BTreeMap::new();
      addr.insert("street".into(), Unknown::String("Main".into()));
      addr.insert("zip".into(), Unknown::I64(12345));
      user.insert("user".into(), Unknown::Object(addr));
      user.insert("id".into(), Unknown::I64(1));
      let err_zip = nested
        .decode_unknown(&Unknown::Object(user))
        .expect_err("zip");
      assert!(err_zip.path.contains("user") && err_zip.path.contains("zip"));

      assert_eq!(
        bool_::<()>().decode_unknown(&Unknown::Bool(false)).unwrap(),
        false
      );
      assert!(bool_::<()>().decode_unknown(&Unknown::Null).is_err());
      assert!(bool_::<()>().decode_unknown(&Unknown::I64(0)).is_err());
      assert_eq!(
        f64::<()>().decode_unknown(&Unknown::F64(2.25)).unwrap(),
        2.25_f64
      );
      assert_eq!(
        f64::<()>().decode_unknown(&Unknown::I64(7)).unwrap(),
        7.0_f64
      );
      assert!(f64::<()>().decode_unknown(&Unknown::Null).is_err());
      assert!(
        f64::<()>()
          .decode_unknown(&Unknown::String("x".into()))
          .is_err()
      );

      let t3s = tuple3(i64::<()>(), string::<()>(), bool_::<()>());
      assert!(
        t3s
          .decode_unknown(&Unknown::Array(vec![Unknown::I64(1), Unknown::I64(2)]))
          .is_err()
      );
      let mut s3miss = BTreeMap::new();
      s3miss.insert("x".into(), Unknown::I64(0));
      s3miss.insert("z".into(), Unknown::I64(3));
      let s3path = struct3("x", i64::<()>(), "y", string::<()>(), "z", i64::<()>());
      let err3 = s3path
        .decode_unknown(&Unknown::Object(s3miss))
        .expect_err("missing y");
      assert!(err3.path.contains("y"));
    }
  }

  mod parse_error_tests {
    use super::*;

    #[test]
    fn prefix_prepends_segment() {
      let e = ParseError::new("field", "bad value");
      let prefixed = e.prefix("root");
      assert_eq!(prefixed.path, "root.field");
    }

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
}
