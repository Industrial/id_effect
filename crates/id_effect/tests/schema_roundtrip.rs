//! Proptest `decode(encode(a)) == a` for every `id_effect::schema` codec combinator
//! (`schema/mod.rs` re-exports from `extra` and `parse`), arities up to 12, and `derive(Schema)`
//! on structs with more than four fields (p-schema-complete).

use std::collections::BTreeMap;

use id_effect::schema::{
  HasSchema, Schema, Unknown, array, bool_, f64, filter, i64, i64_unknown_wire, literal_i64,
  literal_string, null_or, optional, record, refine, string, struct_, struct3, struct4, struct5,
  struct6, struct7, struct8, struct9, struct10, struct11, struct12, suspend, transform, tuple,
  tuple3, tuple4, tuple5, tuple6, tuple7, tuple8, tuple9, tuple10, tuple11, tuple12, union_,
  union_chain, wire_equal,
};
use proptest::prelude::*;

/// Finite doubles only: `NaN != NaN` would falsify any equality law.
fn finite() -> impl Strategy<Value = f64> {
  -1.0e12_f64..1.0e12_f64
}

fn roundtrip<A, I, E>(s: &Schema<A, I, E>, a: A) -> A
where
  A: Clone,
{
  s.decode(s.encode(a))
    .expect("decode(encode(a)) must succeed")
}

// ── primitives & unary combinators ──────────────────────────────────────────

proptest! {
  #[test]
  fn primitives_roundtrip(n in any::<i64>(), s in ".*", b in any::<bool>(), x in finite()) {
    prop_assert_eq!(roundtrip(&i64::<()>(), n), n);
    prop_assert_eq!(roundtrip(&i64_unknown_wire::<()>(), n), n);
    prop_assert_eq!(roundtrip(&string::<()>(), s.clone()), s);
    prop_assert_eq!(roundtrip(&bool_::<()>(), b), b);
    prop_assert_eq!(roundtrip(&f64::<()>(), x), x);
  }

  #[test]
  fn transform_roundtrip(n in any::<i64>()) {
    let s = transform(
      i64::<()>(),
      |n| Ok(n.to_string()),
      |t: String| t.parse().expect("encoded from i64"),
    );
    let t = n.to_string();
    prop_assert_eq!(roundtrip(&s, t.clone()), t);
  }

  #[test]
  fn filter_and_refine_roundtrip(n in 0_i64..) {
    let f = filter(i64::<()>(), |n| *n >= 0, "non-negative");
    let r = refine(i64::<()>(), |n| *n >= 0, "non-negative");
    prop_assert_eq!(roundtrip(&f, n), n);
    prop_assert_eq!(roundtrip(&r, n), n);
  }

  #[test]
  fn optional_and_null_or_roundtrip(o in proptest::option::of(any::<i64>())) {
    prop_assert_eq!(roundtrip(&optional(i64::<()>()), o), o);
    prop_assert_eq!(roundtrip(&null_or(i64::<()>()), o), o);
  }

  #[test]
  fn array_roundtrip(v in prop::collection::vec(".*", 0..16)) {
    prop_assert_eq!(roundtrip(&array(string::<()>()), v.clone()), v);
  }

  #[test]
  fn record_roundtrip(m in prop::collection::btree_map(".*", any::<i64>(), 0..16)) {
    prop_assert_eq!(roundtrip(&record(i64::<()>()), m.clone()), m);
  }

  #[test]
  fn literal_roundtrip(n in any::<i64>(), s in prop::sample::select(vec!["", "a", "kind", "ü"])) {
    prop_assert_eq!(roundtrip(&literal_i64::<()>(n), n), n);
    prop_assert_eq!(roundtrip(&literal_string::<()>(s), s), s);
  }

  #[test]
  fn union_roundtrip(n in any::<i64>()) {
    let neg = || filter(i64_unknown_wire::<()>(), |n| *n < 0, "negative");
    let u = union_(neg(), i64_unknown_wire::<()>());
    let c = union_chain(vec![neg(), literal_wire(0), i64_unknown_wire::<()>()]);
    prop_assert_eq!(roundtrip(&u, n), n);
    prop_assert_eq!(roundtrip(&c, n), n);
  }

  #[test]
  fn suspend_recursive_roundtrip(t in tree_strategy()) {
    prop_assert_eq!(roundtrip(&tree(), t.clone()), t.clone());
    prop_assert_eq!(tree().decode_unknown(&tree().encode(t.clone())).unwrap(), t);
  }

  #[test]
  fn wire_equal_is_reflexive_and_matches_eq(a in any::<i64>(), b in any::<i64>()) {
    let s = i64::<()>();
    prop_assert!(wire_equal(&s, &a, &a));
    prop_assert_eq!(wire_equal(&s, &a, &b), a == b);
  }
}

fn literal_wire(n: i64) -> Schema<i64, Unknown, ()> {
  transform(
    i64_unknown_wire::<()>(),
    move |m| {
      if m == n {
        Ok(m)
      } else {
        Err(id_effect::schema::ParseError::new("", "literal"))
      }
    },
    |m| m,
  )
}

#[derive(Clone, Debug, PartialEq)]
struct Tree(Vec<Tree>);

/// Recursive schema through [`suspend`]: `Tree` ↔ nested [`Unknown::Array`].
fn tree() -> Schema<Tree, Unknown, ()> {
  let kids = array(suspend(tree));
  let k_dec = kids.clone();
  let k_enc = kids.clone();
  Schema::make(
    move |u: Unknown| match u {
      Unknown::Array(v) => k_dec.decode(v).map(Tree),
      _ => Err(id_effect::schema::ParseError::new("", "expected array")),
    },
    move |t: Tree| Unknown::Array(k_enc.encode(t.0)),
    move |u| kids.decode_unknown(u).map(Tree),
  )
}

fn tree_strategy() -> impl Strategy<Value = Tree> {
  Just(Tree(vec![])).prop_recursive(4, 32, 4, |inner| {
    prop::collection::vec(inner, 0..4).prop_map(Tree)
  })
}

// ── tuple / struct arities 2..=12 ───────────────────────────────────────────

// proptest implements `Arbitrary` only up to 10-tuples, but `Strategy` up to 12-tuples, so the
// generators are tuples of strategies rather than `any::<(i64, ..)>()`.
macro_rules! any_i64 {
  ($x:ident) => {
    any::<i64>()
  };
}
macro_rules! sch_i64 {
  ($x:ident) => {
    i64::<()>()
  };
}
macro_rules! named_i64 {
  ($x:ident) => {
    (stringify!($x), i64::<()>())
  };
}

macro_rules! tuple_arity {
  ($test:ident, $comb:ident; $($x:ident),+) => {
    proptest! {
      #[test]
      fn $test(v in ($( any_i64!($x), )+)) {
        let s = $comb($( sch_i64!($x) ),+);
        prop_assert_eq!(roundtrip(&s, v), v);
      }
    }
  };
}

macro_rules! struct_arity {
  ($test:ident, $comb:ident; $($x:ident),+) => {
    proptest! {
      #[test]
      fn $test(v in ($( any_i64!($x), )+)) {
        let s = $comb($( named_i64!($x).0, named_i64!($x).1 ),+);
        prop_assert_eq!(roundtrip(&s, v), v);
      }
    }
  };
}

tuple_arity!(tuple2_roundtrip, tuple; a, b);
tuple_arity!(tuple3_roundtrip, tuple3; a, b, c);
tuple_arity!(tuple4_roundtrip, tuple4; a, b, c, d);
tuple_arity!(tuple5_roundtrip, tuple5; a, b, c, d, e);
tuple_arity!(tuple6_roundtrip, tuple6; a, b, c, d, e, f);
tuple_arity!(tuple7_roundtrip, tuple7; a, b, c, d, e, f, g);
tuple_arity!(tuple8_roundtrip, tuple8; a, b, c, d, e, f, g, h);
tuple_arity!(tuple9_roundtrip, tuple9; a, b, c, d, e, f, g, h, i);
tuple_arity!(tuple10_roundtrip, tuple10; a, b, c, d, e, f, g, h, i, j);
tuple_arity!(tuple11_roundtrip, tuple11; a, b, c, d, e, f, g, h, i, j, k);
tuple_arity!(tuple12_roundtrip, tuple12; a, b, c, d, e, f, g, h, i, j, k, l);

struct_arity!(struct2_roundtrip, struct_; a, b);
struct_arity!(struct3_roundtrip, struct3; a, b, c);
struct_arity!(struct4_roundtrip, struct4; a, b, c, d);
struct_arity!(struct5_roundtrip, struct5; a, b, c, d, e);
struct_arity!(struct6_roundtrip, struct6; a, b, c, d, e, f);
struct_arity!(struct7_roundtrip, struct7; a, b, c, d, e, f, g);
struct_arity!(struct8_roundtrip, struct8; a, b, c, d, e, f, g, h);
struct_arity!(struct9_roundtrip, struct9; a, b, c, d, e, f, g, h, i);
struct_arity!(struct10_roundtrip, struct10; a, b, c, d, e, f, g, h, i, j);
struct_arity!(struct11_roundtrip, struct11; a, b, c, d, e, f, g, h, i, j, k);
struct_arity!(struct12_roundtrip, struct12; a, b, c, d, e, f, g, h, i, j, k, l);

type Mixed12 = (
  i64,
  String,
  bool,
  f64,
  Option<i64>,
  Vec<i64>,
  BTreeMap<String, bool>,
  String,
  bool,
  f64,
  Option<String>,
  Vec<bool>,
);

fn mixed12() -> impl Strategy<Value = Mixed12> {
  (
    any::<i64>(),
    ".*",
    any::<bool>(),
    finite(),
    proptest::option::of(any::<i64>()),
    prop::collection::vec(any::<i64>(), 0..6),
    prop::collection::btree_map(".*", any::<bool>(), 0..4),
    ".*",
    any::<bool>(),
    finite(),
    proptest::option::of(".*"),
    prop::collection::vec(any::<bool>(), 0..6),
  )
}

/// Wire of an all-`Unknown` 12-ary codec as the [`Unknown`] tree `decode_unknown` reads.
fn unknown12(w: [Unknown; 12]) -> Vec<Unknown> {
  w.into_iter().collect()
}

proptest! {
  #[test]
  fn tuple12_mixed_roundtrip(v in mixed12()) {
    let s = tuple12(
      i64::<()>(), string(), bool_(), f64(), optional(i64()), array(i64()),
      record(bool_()), string(), bool_(), f64(), optional(string()), array(bool_()),
    );
    prop_assert_eq!(roundtrip(&s, v.clone()), v);
  }

  #[test]
  fn struct12_mixed_roundtrip(v in mixed12()) {
    let s = struct12(
      "a", i64::<()>(), "b", string(), "c", bool_(), "d", f64(), "e", optional(i64()),
      "f", array(i64()), "g", record(bool_()), "h", string(), "i", bool_(), "j", f64(),
      "k", optional(string()), "l", array(bool_()),
    );
    prop_assert_eq!(roundtrip(&s, v.clone()), v);
  }

  #[test]
  fn arity12_decode_unknown_agrees(v in (
    any::<i64>(), any::<i64>(), any::<i64>(), any::<i64>(), any::<i64>(), any::<i64>(),
    any::<i64>(), any::<i64>(), any::<i64>(), any::<i64>(), any::<i64>(), any::<i64>(),
  )) {
    let u = || i64_unknown_wire::<()>();
    let t = tuple12(u(), u(), u(), u(), u(), u(), u(), u(), u(), u(), u(), u());
    let w = t.encode(v);
    let parts = unknown12([w.0, w.1, w.2, w.3, w.4, w.5, w.6, w.7, w.8, w.9, w.10, w.11]);
    prop_assert_eq!(t.decode_unknown(&Unknown::Array(parts.clone())).unwrap(), v);

    let names = ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l"];
    let s = struct12(
      "a", u(), "b", u(), "c", u(), "d", u(), "e", u(), "f", u(),
      "g", u(), "h", u(), "i", u(), "j", u(), "k", u(), "l", u(),
    );
    let obj: BTreeMap<String, Unknown> =
      names.iter().map(|n| n.to_string()).zip(parts).collect();
    prop_assert_eq!(s.decode_unknown(&Unknown::Object(obj.clone())).unwrap(), v);

    let mut missing = obj;
    missing.remove("l");
    let err = s.decode_unknown(&Unknown::Object(missing)).unwrap_err();
    prop_assert_eq!(err.path, "l");
  }
}

#[test]
fn arity12_errors_carry_position_and_name() {
  let pos = filter(i64::<()>(), |n| *n >= 0, "non-negative");
  let t = tuple12(
    i64::<()>(),
    i64(),
    i64(),
    i64(),
    i64(),
    i64(),
    i64(),
    i64(),
    i64(),
    i64(),
    i64(),
    pos.clone(),
  );
  let err = t.decode((0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, -1)).unwrap_err();
  assert_eq!(err.path, "11");
  assert!(
    t.decode_unknown(&Unknown::Array(vec![Unknown::I64(1)]))
      .is_err()
  );

  let s = struct12(
    "a",
    i64::<()>(),
    "b",
    i64(),
    "c",
    i64(),
    "d",
    i64(),
    "e",
    i64(),
    "f",
    i64(),
    "g",
    i64(),
    "h",
    i64(),
    "i",
    i64(),
    "j",
    i64(),
    "k",
    i64(),
    "last",
    pos,
  );
  let err = s.decode((0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, -1)).unwrap_err();
  assert_eq!(err.path, "last");
  assert!(s.decode_unknown(&Unknown::I64(0)).is_err());
}

// ── derive(Schema) with more than four fields ───────────────────────────────

#[derive(Clone, Debug, PartialEq, id_effect::Schema)]
struct Inner {
  label: String,
  weight: i32,
}

#[derive(Clone, Debug, PartialEq, id_effect::Schema)]
struct Wide {
  #[schema("user_name")]
  name: String,
  active: bool,
  count: i64,
  small: i32,
  ratio: f64,
  nick: Option<String>,
  score: Option<i64>,
  flag: Option<bool>,
  temp: Option<f64>,
  level: Option<i32>,
  tags: Vec<String>,
  ids: Vec<i64>,
  smalls: Vec<i32>,
  bits: Vec<bool>,
  samples: Vec<f64>,
  inner: Inner,
  maybe_inner: Option<Inner>,
  inners: Vec<Inner>,
}

fn inner_strategy() -> impl Strategy<Value = Inner> {
  (".*", any::<i32>()).prop_map(|(label, weight)| Inner { label, weight })
}

fn wide_strategy() -> impl Strategy<Value = Wide> {
  let head = (
    ".*",
    any::<bool>(),
    any::<i64>(),
    any::<i32>(),
    finite(),
    proptest::option::of(".*"),
    proptest::option::of(any::<i64>()),
    proptest::option::of(any::<bool>()),
    proptest::option::of(finite()),
    proptest::option::of(any::<i32>()),
  );
  let tail = (
    prop::collection::vec(".*", 0..4),
    prop::collection::vec(any::<i64>(), 0..4),
    prop::collection::vec(any::<i32>(), 0..4),
    prop::collection::vec(any::<bool>(), 0..4),
    prop::collection::vec(finite(), 0..4),
    inner_strategy(),
    proptest::option::of(inner_strategy()),
    prop::collection::vec(inner_strategy(), 0..3),
  );
  (head, tail).prop_map(
    |(
      (name, active, count, small, ratio, nick, score, flag, temp, level),
      (tags, ids, smalls, bits, samples, inner, maybe_inner, inners),
    )| Wide {
      name,
      active,
      count,
      small,
      ratio,
      nick,
      score,
      flag,
      temp,
      level,
      tags,
      ids,
      smalls,
      bits,
      samples,
      inner,
      maybe_inner,
      inners,
    },
  )
}

proptest! {
  #[test]
  fn derive_wide_struct_roundtrip(w in wide_strategy()) {
    let s = Wide::schema();
    let wire = s.encode(w.clone());
    prop_assert_eq!(s.decode(wire.clone()).unwrap(), w.clone());
    prop_assert_eq!(s.decode_unknown(&wire).unwrap(), w.clone());
    prop_assert_eq!(<Wide as HasSchema>::schema().decode(wire).unwrap(), w);
  }
}

#[test]
fn derive_wide_struct_wire_shape_and_errors() {
  let wire = Wide::schema().encode(Wide {
    name: "n".into(),
    active: true,
    count: 1,
    small: -2,
    ratio: 0.5,
    nick: None,
    score: Some(3),
    flag: None,
    temp: Some(1.5),
    level: None,
    tags: vec!["t".into()],
    ids: vec![],
    smalls: vec![7],
    bits: vec![false],
    samples: vec![],
    inner: Inner {
      label: "l".into(),
      weight: 9,
    },
    maybe_inner: None,
    inners: vec![],
  });
  let Unknown::Object(mut obj) = wire else {
    panic!("derive(Schema) encodes to an object");
  };
  assert_eq!(obj.len(), 18);
  assert_eq!(obj.get("user_name"), Some(&Unknown::String("n".into())));
  assert!(!obj.contains_key("name"));
  assert_eq!(obj.get("small"), Some(&Unknown::I64(-2)));

  obj.insert("small".into(), Unknown::I64(i64::from(i32::MAX) + 1));
  let err = Wide::schema()
    .decode_unknown(&Unknown::Object(obj.clone()))
    .unwrap_err();
  assert_eq!(err.path, "small");

  obj.remove("count");
  let err = Wide::schema()
    .decode_unknown(&Unknown::Object(obj))
    .unwrap_err();
  assert_eq!(err.path, "count");
}
