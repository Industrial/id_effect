//! Additional combinators: [`record`], [`suspend`] / [`suspend_named`], [`union_chain`], literals,
//! [`wire_equal`].
//!
//! See [`crate::schema::SPEC.md`](./SPEC.md) and repository [`TESTING.md`](../../../../TESTING.md).

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use crate::schema::ast::{LazyAst, SchemaAst, SchemaKind};
use crate::schema::data::EffectData;
use crate::schema::parse::{Issues, ParseError, Schema, Unknown, union_ast};
use crate::schema::parse_errors::ParseErrors;

/// Homogeneous string-keyed map (JSON object with arbitrary keys, same value schema).
///
/// Wire type is [`BTreeMap`] for deterministic encoding order.
pub fn record<A, I, E>(
  value: Schema<A, I, E>,
) -> Schema<BTreeMap<String, A>, BTreeMap<String, I>, E>
where
  E: EffectData + 'static,
  A: 'static,
  I: 'static,
{
  let v_dec = value.clone();
  let v_enc = value.clone();
  let v_du = value.clone();
  Schema::make(
    move |m: BTreeMap<String, I>| {
      let mut out = BTreeMap::new();
      for (k, i) in m {
        let a = v_dec.decode(i).map_err(|e| e.prefix(k.as_str()))?;
        out.insert(k, a);
      }
      Ok(out)
    },
    move |m: BTreeMap<String, A>| m.into_iter().map(|(k, a)| (k, v_enc.encode(a))).collect(),
    move |u| {
      let obj = match u {
        Unknown::Object(m) => m,
        _ => return Err(ParseError::new("", "expected object for record")),
      };
      let mut out = BTreeMap::new();
      for (k, uv) in obj {
        let a = v_du.decode_unknown(uv).map_err(|e| e.prefix(k.as_str()))?;
        out.insert(k.clone(), a);
      }
      Ok(out)
    },
  )
  .with_ast(SchemaAst::new(SchemaKind::Record(Box::new(
    value.ast().clone(),
  ))))
  .with_decode_unknown_all(move |u| {
    let obj = match u {
      Unknown::Object(m) => m,
      _ => {
        return Err(ParseErrors::one(ParseError::new(
          "",
          "expected object for record",
        )));
      }
    };
    let mut issues = Issues::default();
    let mut out = BTreeMap::new();
    for (k, uv) in obj {
      if let Some(a) = issues.take(value.decode_unknown_all(uv).map_err(|e| e.prefix(k))) {
        out.insert(k.clone(), a);
      }
    }
    if issues.is_empty() {
      Ok(out)
    } else {
      Err(issues.into_errors())
    }
  })
}

/// Exact string literal (Effect.ts `Schema.Literal` for strings).
pub fn literal_string<E: EffectData + 'static>(
  s: &'static str,
) -> Schema<&'static str, &'static str, E> {
  let s2 = s;
  Schema::make(
    move |w: &'static str| {
      if w == s {
        Ok(s)
      } else {
        Err(ParseError::new("", format!("expected literal {s:?}")))
      }
    },
    |a| a,
    move |u| match u {
      Unknown::String(t) if t == s2 => Ok(s2),
      Unknown::Null => Err(ParseError::new("", "expected literal string, got null")),
      _ => Err(ParseError::new(
        "",
        format!("expected literal string {s2:?}"),
      )),
    },
  )
  .with_ast(SchemaAst::new(SchemaKind::Literal(Unknown::String(
    s.to_owned(),
  ))))
}

/// Exact `i64` literal.
pub fn literal_i64<E: EffectData + 'static>(n: i64) -> Schema<i64, i64, E> {
  Schema::make(
    move |w: i64| {
      if w == n {
        Ok(n)
      } else {
        Err(ParseError::new("", format!("expected literal {n}")))
      }
    },
    |a| a,
    move |u| match u {
      Unknown::I64(x) if *x == n => Ok(n),
      Unknown::Null => Err(ParseError::new("", "expected literal i64, got null")),
      _ => Err(ParseError::new("", format!("expected literal i64 {n}"))),
    },
  )
  .with_ast(SchemaAst::new(SchemaKind::Literal(Unknown::I64(n))))
}

/// Alias of [`crate::schema::parse::optional`] — nullable / optional root (`null` → `None`).
pub fn null_or<A, I, E>(s: Schema<A, I, E>) -> Schema<Option<A>, Option<I>, E>
where
  E: EffectData + 'static,
  A: 'static,
  I: 'static,
{
  crate::schema::parse::optional(s)
}

/// Lazy schema for recursion (Effect.ts `Schema.suspend`): `thunk` is called at most once to build
/// the real [`Schema`], then results are cached.
///
/// Its [`SchemaAst`] is an unnamed [`SchemaKind::Lazy`]; prefer [`suspend_named`] for recursive
/// schemas you export, so JSON Schema can refer back with `$ref`.
pub fn suspend<A, I, E, F>(thunk: F) -> Schema<A, I, E>
where
  E: EffectData + 'static,
  A: 'static,
  I: 'static,
  F: Fn() -> Schema<A, I, E> + Send + Sync + 'static,
{
  suspend_impl(None, thunk)
}

/// [`suspend`] with a definition `name`: JSON Schema export emits the target once under
/// `$defs/<name>` and refers to it with `$ref`, so recursive schemas export finitely.
pub fn suspend_named<A, I, E, F>(name: impl Into<String>, thunk: F) -> Schema<A, I, E>
where
  E: EffectData + 'static,
  A: 'static,
  I: 'static,
  F: Fn() -> Schema<A, I, E> + Send + Sync + 'static,
{
  suspend_impl(Some(name.into()), thunk)
}

fn suspend_impl<A, I, E, F>(name: Option<String>, thunk: F) -> Schema<A, I, E>
where
  E: EffectData + 'static,
  A: 'static,
  I: 'static,
  F: Fn() -> Schema<A, I, E> + Send + Sync + 'static,
{
  let thunk = Arc::new(thunk);
  let cell: Arc<OnceLock<Schema<A, I, E>>> = Arc::new(OnceLock::new());
  let t1 = thunk.clone();
  let c1 = cell.clone();
  let t2 = thunk.clone();
  let c2 = cell.clone();
  let t3 = thunk.clone();
  let c3 = cell.clone();
  Schema::make(
    move |i| {
      let s = c1.get_or_init(|| t1());
      s.decode(i)
    },
    move |a| {
      let s = c2.get_or_init(|| t2());
      s.encode(a)
    },
    move |u| {
      let s = c3.get_or_init(|| t3());
      s.decode_unknown(u)
    },
  )
  .with_ast(SchemaAst::new(SchemaKind::Lazy(LazyAst::new(name, {
    let t = thunk.clone();
    let c = cell.clone();
    move || c.get_or_init(|| t()).ast().clone()
  }))))
  .with_decode_unknown_all(move |u| cell.get_or_init(|| thunk()).decode_unknown_all(u))
}

/// Try each schema in order; first successful decode wins (encode uses **only** the first schema).
///
/// All arms must share semantic type `A` and wire [`Unknown`]. For **encode**, the **first**
/// schema is used — arms should agree on encoding when possible.
pub fn union_chain<A, E>(schemas: Vec<Schema<A, Unknown, E>>) -> Schema<A, Unknown, E>
where
  E: EffectData + 'static,
  A: 'static,
{
  assert!(
    !schemas.is_empty(),
    "union_chain: at least one schema is required"
  );
  let list: Arc<Vec<Schema<A, Unknown, E>>> = Arc::new(schemas);
  let list_dec = list.clone();
  let list_du = list.clone();
  let list_all = list.clone();
  let enc = list[0].clone();
  let ast = union_ast(list.iter().map(Schema::ast));
  Schema::make(
    move |u: Unknown| {
      for s in list_dec.iter() {
        if let Ok(a) = s.decode(u.clone()) {
          return Ok(a);
        }
      }
      Err(ParseError::new(
        "",
        "no arm of union_chain accepted the wire value",
      ))
    },
    move |a| enc.encode(a),
    move |u| {
      for s in list_du.iter() {
        if let Ok(a) = s.decode_unknown(u) {
          return Ok(a);
        }
      }
      Err(ParseError::new(
        "",
        "no arm of union_chain accepted the unknown value",
      ))
    },
  )
  .with_ast(ast)
  .with_decode_unknown_all(move |u| {
    for s in list_all.iter() {
      if let Ok(a) = s.decode_unknown_all(u) {
        return Ok(a);
      }
    }
    Err(ParseErrors::one(ParseError::new(
      "",
      "no arm of union_chain accepted the unknown value",
    )))
  })
}

/// Compare two values by **encoded wire** equality (Effect.ts-style equivalence when `I` matches).
pub fn wire_equal<A, I, E>(s: &Schema<A, I, E>, x: &A, y: &A) -> bool
where
  A: Clone,
  I: PartialEq,
{
  s.encode(x.clone()) == s.encode(y.clone())
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::schema::parse::{i64, string};

  mod record_codec {
    use super::*;

    #[test]
    fn decode_wire_map_success() {
      let s = record(i64::<()>());
      let mut m = std::collections::BTreeMap::new();
      m.insert("x".to_string(), 10_i64);
      m.insert("y".to_string(), 20_i64);
      let got = s.decode(m.clone()).expect("decode");
      assert_eq!(got, m);
    }

    #[test]
    fn decode_wire_map_error_prefixes_key() {
      use crate::schema::parse::filter;
      let s = record(filter(i64::<()>(), |n| *n > 0, "must be positive"));
      let mut m = std::collections::BTreeMap::new();
      m.insert("k".to_string(), -1_i64);
      let err = s.decode(m).expect_err("negative should fail");
      assert!(err.path.contains("k"), "path was {:?}", err.path);
    }

    #[test]
    fn decode_unknown_object_to_map() {
      let s = record(i64::<()>());
      let mut m = BTreeMap::new();
      m.insert("a".into(), Unknown::I64(1));
      m.insert("b".into(), Unknown::I64(2));
      let got = s.decode_unknown(&Unknown::Object(m)).expect("ok");
      let mut e = BTreeMap::new();
      e.insert("a".into(), 1_i64);
      e.insert("b".into(), 2_i64);
      assert_eq!(got, e);
    }

    #[test]
    fn encode_round_trip_sorted_keys() {
      let s = record(string::<()>());
      let mut m = BTreeMap::new();
      m.insert("z".into(), "z".to_string());
      m.insert("a".into(), "a".to_string());
      let dec = s.decode(m.clone()).expect("decode");
      assert_eq!(s.encode(dec), m);
    }
  }

  mod literal_codec {
    use super::*;

    #[test]
    fn literal_string_accepts_exact_match() {
      let s = literal_string::<()>("ok");
      assert_eq!(
        s.decode_unknown(&Unknown::String("ok".into())).unwrap(),
        "ok"
      );
    }

    #[test]
    fn literal_string_decode_wire_error_on_wrong_value() {
      let s = literal_string::<()>("hello");
      assert!(s.decode("world").is_err());
    }

    #[test]
    fn literal_string_decode_wire_success_on_exact_value() {
      let s = literal_string::<()>("exact");
      assert_eq!(s.decode("exact").unwrap(), "exact");
    }

    #[test]
    fn literal_string_encode_returns_value() {
      let s = literal_string::<()>("hi");
      assert_eq!(s.encode("hi"), "hi");
    }

    #[test]
    fn literal_string_decode_unknown_null_fails() {
      let s = literal_string::<()>("x");
      assert!(s.decode_unknown(&Unknown::Null).is_err());
    }

    #[test]
    fn literal_string_decode_unknown_wrong_string_fails() {
      let s = literal_string::<()>("expected");
      assert!(s.decode_unknown(&Unknown::String("other".into())).is_err());
    }

    #[test]
    fn literal_i64_accepts_exact_match() {
      let s = literal_i64::<()>(42);
      assert_eq!(s.decode(42).unwrap(), 42);
    }

    #[test]
    fn literal_i64_decode_wire_error_on_wrong_value() {
      let s = literal_i64::<()>(10);
      assert!(s.decode(11).is_err());
    }

    #[test]
    fn literal_i64_encode_returns_value() {
      let s = literal_i64::<()>(7);
      assert_eq!(s.encode(7), 7);
    }

    #[test]
    fn literal_i64_decode_unknown_exact_match() {
      let s = literal_i64::<()>(5);
      assert_eq!(s.decode_unknown(&Unknown::I64(5)).unwrap(), 5);
    }

    #[test]
    fn literal_i64_decode_unknown_null_fails() {
      let s = literal_i64::<()>(5);
      assert!(s.decode_unknown(&Unknown::Null).is_err());
    }

    #[test]
    fn literal_i64_decode_unknown_wrong_type_fails() {
      let s = literal_i64::<()>(5);
      assert!(s.decode_unknown(&Unknown::String("5".into())).is_err());
    }

    #[test]
    fn literal_i64_rejects_wrong_number() {
      let s = literal_i64::<()>(42);
      assert!(s.decode_unknown(&Unknown::I64(41)).is_err());
    }
  }

  mod suspend_codec {
    use super::*;

    #[test]
    fn suspend_initializes_once() {
      use std::sync::atomic::{AtomicUsize, Ordering};
      let count = Arc::new(AtomicUsize::new(0));
      let c2 = count.clone();
      let s: Schema<i64, i64, ()> = suspend(move || {
        c2.fetch_add(1, Ordering::SeqCst);
        i64()
      });
      assert_eq!(s.decode(1_i64).unwrap(), 1);
      assert_eq!(s.decode(2_i64).unwrap(), 2);
      assert_eq!(count.load(Ordering::SeqCst), 1);
    }
  }

  mod null_or_codec {
    use super::*;

    #[test]
    fn null_or_decodes_null_as_none() {
      let s = null_or(i64::<()>());
      assert_eq!(s.decode_unknown(&Unknown::Null).unwrap(), None);
    }

    #[test]
    fn null_or_decodes_value_as_some() {
      let s = null_or(i64::<()>());
      assert_eq!(s.decode_unknown(&Unknown::I64(99)).unwrap(), Some(99_i64));
    }

    #[test]
    fn null_or_encode_none_is_none() {
      let s = null_or(i64::<()>());
      assert_eq!(s.encode(None), None);
    }

    #[test]
    fn null_or_encode_some_is_some() {
      let s = null_or(i64::<()>());
      assert_eq!(s.encode(Some(3_i64)), Some(3_i64));
    }
  }

  mod union_chain_codec {
    use super::*;
    use crate::schema::parse::{filter, i64_unknown_wire};

    #[test]
    fn first_matching_arm_wins() {
      let s = union_chain(vec![
        filter(i64_unknown_wire::<()>(), |n| *n == 99, "only 99"),
        i64_unknown_wire::<()>(),
      ]);
      assert_eq!(s.decode_unknown(&Unknown::I64(5)).expect("second arm"), 5);
      assert_eq!(s.decode_unknown(&Unknown::I64(99)).expect("first arm"), 99);
    }

    #[test]
    fn union_chain_decode_wire_uses_first_matching_arm() {
      let s = union_chain(vec![
        filter(i64_unknown_wire::<()>(), |n| *n > 0, "positive"),
        i64_unknown_wire::<()>(),
      ]);
      // decode wire (Unknown input)
      assert_eq!(s.decode(Unknown::I64(10)).expect("first arm"), 10);
      assert_eq!(s.decode(Unknown::I64(-5)).expect("second arm"), -5);
    }

    #[test]
    fn union_chain_encode_uses_first_schema() {
      let s = union_chain(vec![i64_unknown_wire::<()>()]);
      assert_eq!(s.encode(42_i64), Unknown::I64(42));
    }

    #[test]
    fn union_chain_decode_unknown_all_fail_returns_error() {
      let s = union_chain(vec![filter(i64_unknown_wire::<()>(), |_| false, "never")]);
      assert!(s.decode_unknown(&Unknown::I64(1)).is_err());
    }

    #[test]
    fn union_chain_decode_wire_all_fail_returns_error() {
      let s = union_chain(vec![filter(i64_unknown_wire::<()>(), |_| false, "never")]);
      assert!(s.decode(Unknown::I64(1)).is_err());
    }
  }

  mod literal_schemas {
    use super::*;

    #[test]
    fn literal_string_schema() {
      let s = literal_string::<()>("fixed");
      assert_eq!(s.decode("fixed").unwrap(), "fixed");
      assert!(s.decode("other").is_err());
    }

    #[test]
    fn literal_i64_schema() {
      let s = literal_i64::<()>(7);
      assert_eq!(s.decode(7).unwrap(), 7);
      assert!(s.decode(8).is_err());
    }
  }

  mod wire_equal_fn {
    use super::*;

    #[test]
    fn compares_encoded_wire() {
      let s = i64::<()>();
      assert!(wire_equal(&s, &1, &1));
      assert!(!wire_equal(&s, &1, &2));
    }
  }
}
