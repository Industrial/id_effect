//! `#[derive(Schema)]` on structs with more than four fields, every supported field shape
//! (scalars, `i32`, `Option<_>`, `Vec<_>`, nested `HasSchema`), round-tripping over `Unknown`.

use id_effect::schema::{HasSchema, Unknown};
use id_effect_proc_macro::Schema;

#[derive(Clone, Debug, PartialEq, Schema)]
struct Leaf {
  id: i32,
}

#[derive(Clone, Debug, PartialEq, Schema)]
struct Wide {
  a: String,
  b: bool,
  c: i64,
  d: i32,
  e: f64,
  #[schema("f_wire")]
  f: Option<String>,
  g: Option<i64>,
  h: Option<bool>,
  i: Option<f64>,
  j: Option<i32>,
  k: Vec<String>,
  l: Vec<i64>,
  m: Vec<i32>,
  n: Vec<bool>,
  o: Vec<f64>,
  p: Leaf,
  q: Option<Leaf>,
  r: Vec<Leaf>,
}

fn sample(some: bool) -> Wide {
  Wide {
    a: "a".into(),
    b: true,
    c: i64::MIN,
    d: i32::MIN,
    e: -0.5,
    f: some.then(|| "f".into()),
    g: some.then_some(7),
    h: some.then_some(false),
    i: some.then_some(2.25),
    j: some.then_some(i32::MAX),
    k: vec!["x".into(), "y".into()],
    l: vec![1, 2, 3],
    m: vec![-1, 0, 1],
    n: vec![true, false],
    o: vec![1.0, -1.0],
    p: Leaf { id: 1 },
    q: some.then_some(Leaf { id: 2 }),
    r: vec![Leaf { id: 3 }, Leaf { id: 4 }],
  }
}

#[test]
fn wide_derive_roundtrips_with_and_without_options() {
  for some in [true, false] {
    let v = sample(some);
    let s = <Wide as HasSchema>::schema();
    let wire = s.encode(v.clone());
    assert_eq!(s.decode(wire.clone()).unwrap(), v);
    assert_eq!(Wide::schema().decode_unknown(&wire).unwrap(), v);
  }
}

#[test]
fn wide_derive_wire_keys_and_errors() {
  let Unknown::Object(mut obj) = Wide::schema().encode(sample(true)) else {
    panic!("expected object wire");
  };
  assert_eq!(obj.len(), 18);
  assert!(obj.contains_key("f_wire") && !obj.contains_key("f"));
  assert_eq!(obj.get("d"), Some(&Unknown::I64(i64::from(i32::MIN))));

  obj.insert("m".into(), Unknown::Array(vec![Unknown::I64(i64::MAX)]));
  let err = Wide::schema()
    .decode_unknown(&Unknown::Object(obj.clone()))
    .unwrap_err();
  assert!(err.path.starts_with('m'), "path was {:?}", err.path);

  obj.remove("q");
  obj.insert("m".into(), Unknown::Array(vec![]));
  let decoded = Wide::schema()
    .decode_unknown(&Unknown::Object(obj.clone()))
    .unwrap();
  assert_eq!(decoded.q, None, "missing Option field decodes as None");

  obj.remove("e");
  let err = Wide::schema()
    .decode_unknown(&Unknown::Object(obj))
    .unwrap_err();
  assert_eq!(err.path, "e");
}
