//! `Schema::decode_unknown_all` — one pass, every field / index / key failure reported
//! (data.schema#6), plus built-in validators inside accumulated structs (data.schema#8).
//!
//! Law: on success it equals `decode_unknown`; on failure its **first** issue is exactly the
//! fail-fast `decode_unknown` error, so the existing error shape is unchanged.

use std::collections::BTreeMap;

use id_effect::schema::{
  ParseError, ParseErrors, Schema, Unknown, array, email, f64, i64, i64_unknown_wire, length,
  optional, range, record, string, struct_, struct3, struct5, suspend, transform, tuple, tuple5,
  union_, union_chain,
};
use proptest::prelude::*;

fn obj(entries: &[(&str, Unknown)]) -> Unknown {
  Unknown::Object(
    entries
      .iter()
      .map(|(k, v)| ((*k).to_owned(), v.clone()))
      .collect::<BTreeMap<_, _>>(),
  )
}

fn s(v: &str) -> Unknown {
  Unknown::String(v.to_owned())
}

fn issues(e: &ParseErrors) -> Vec<(String, String)> {
  e.issues
    .iter()
    .map(|i| (i.path.clone(), i.message.clone()))
    .collect()
}

fn pair(e: &ParseError) -> (String, String) {
  (e.path.clone(), e.message.clone())
}

fn signup() -> Schema<(String, String, i64), (String, String, i64), ()> {
  struct3(
    "name",
    length(string::<()>(), 2, 20),
    "email",
    email(string::<()>()),
    "age",
    range(i64::<()>(), 0, 150),
  )
}

#[test]
fn struct_reports_every_failing_field_in_declaration_order() {
  let input = obj(&[
    ("name", s("x")),
    ("email", s("nope")),
    ("age", Unknown::I64(200)),
  ]);
  let errs = signup().decode_unknown_all(&input).unwrap_err();
  assert_eq!(
    issues(&errs),
    vec![
      ("name".into(), "must be at least 2 characters".into()),
      ("email".into(), "must be a valid email address".into()),
      ("age".into(), "must be <= 150".into()),
    ]
  );
  // Fail-fast path is unchanged: first field only.
  let first = signup().decode_unknown(&input).unwrap_err();
  assert_eq!(pair(&first), issues(&errs)[0]);
}

#[test]
fn missing_and_invalid_fields_are_both_reported() {
  let input = obj(&[("email", Unknown::I64(1))]);
  let errs = signup().decode_unknown_all(&input).unwrap_err();
  assert_eq!(
    issues(&errs),
    vec![
      ("name".into(), "missing field name".into()),
      ("email".into(), "expected string".into()),
      ("age".into(), "missing field age".into()),
    ]
  );
}

#[test]
fn valid_input_decodes_like_decode_unknown() {
  let input = obj(&[
    ("name", s("Ada")),
    ("email", s("ada@example.com")),
    ("age", Unknown::I64(36)),
  ]);
  let all = signup().decode_unknown_all(&input).unwrap();
  assert_eq!(all, signup().decode_unknown(&input).unwrap());
}

#[test]
fn nested_structs_arrays_and_records_prefix_paths() {
  let item = struct_(
    "sku",
    length(string::<()>(), 3, 3),
    "qty",
    range(i64::<()>(), 1, 99),
  );
  let order = struct3(
    "id",
    i64::<()>(),
    "items",
    array(item),
    "tags",
    record(string::<()>()),
  );
  let input = obj(&[
    ("id", s("not-a-number")),
    (
      "items",
      Unknown::Array(vec![
        obj(&[("sku", s("abc")), ("qty", Unknown::I64(1))]),
        obj(&[("sku", s("toolong")), ("qty", Unknown::I64(0))]),
        obj(&[("qty", Unknown::I64(5))]),
      ]),
    ),
    (
      "tags",
      obj(&[("a", Unknown::I64(1)), ("b", s("ok")), ("c", Unknown::Null)]),
    ),
  ]);
  let errs = order.decode_unknown_all(&input).unwrap_err();
  assert_eq!(
    issues(&errs),
    vec![
      ("id".into(), "expected i64".into()),
      ("items.1.sku".into(), "must be at most 3 characters".into()),
      ("items.1.qty".into(), "must be >= 1".into()),
      ("items.2.sku".into(), "missing field sku".into()),
      ("tags.a".into(), "expected string".into()),
      ("tags.c".into(), "expected string, got null".into()),
    ]
  );
  assert_eq!(
    pair(&order.decode_unknown(&input).unwrap_err()),
    issues(&errs)[0]
  );
}

#[test]
fn tuples_and_wide_structs_accumulate() {
  let t = tuple(i64::<()>(), string::<()>());
  let errs = t
    .decode_unknown_all(&Unknown::Array(vec![s("x"), Unknown::I64(1)]))
    .unwrap_err();
  assert_eq!(errs.issues.len(), 2);
  assert_eq!(errs.issues[1].path, "1");

  let t5 = tuple5(i64(), i64(), i64(), i64(), i64::<()>());
  let errs = t5
    .decode_unknown_all(&Unknown::Array(vec![Unknown::Null; 5]))
    .unwrap_err();
  assert_eq!(errs.issues.len(), 5);

  let s5 = struct5(
    "a",
    i64(),
    "b",
    i64(),
    "c",
    i64(),
    "d",
    i64(),
    "e",
    i64::<()>(),
  );
  let errs = s5
    .decode_unknown_all(&obj(&[("c", Unknown::I64(1))]))
    .unwrap_err();
  let paths: Vec<_> = errs.issues.iter().map(|i| i.path.as_str()).collect();
  assert_eq!(paths, ["a", "b", "d", "e"]);
}

#[test]
fn non_container_root_is_a_single_issue() {
  let errs = signup().decode_unknown_all(&Unknown::I64(1)).unwrap_err();
  assert_eq!(
    issues(&errs),
    vec![(String::new(), "expected object".into())]
  );
}

#[test]
fn optional_union_transform_and_suspend_delegate() {
  let opt = optional(struct_("a", i64::<()>(), "b", i64::<()>()));
  assert_eq!(opt.decode_unknown_all(&Unknown::Null).unwrap(), None);
  assert_eq!(
    opt.decode_unknown_all(&obj(&[])).unwrap_err().issues.len(),
    2
  );

  let u = union_(i64_unknown_wire::<()>(), i64_unknown_wire::<()>());
  assert_eq!(u.decode_unknown_all(&Unknown::I64(3)).unwrap(), 3);
  assert!(u.decode_unknown_all(&Unknown::Null).is_err());
  let uc = union_chain(vec![i64_unknown_wire::<()>()]);
  assert_eq!(
    uc.decode_unknown_all(&s("x")).unwrap_err().issues[0].message,
    "no arm of union_chain accepted the unknown value"
  );

  let tr = transform(
    struct_("x", f64::<()>(), "y", f64::<()>()),
    |(x, y)| Ok(x + y),
    |s: f64| (s, 0.0),
  );
  assert_eq!(
    tr.decode_unknown_all(&obj(&[("x", Unknown::F64(1.0)), ("y", Unknown::I64(2))]))
      .unwrap(),
    3.0
  );
  assert_eq!(
    tr.decode_unknown_all(&obj(&[])).unwrap_err().issues.len(),
    2
  );

  let lazy: Schema<Vec<i64>, Vec<i64>, ()> = suspend(|| array(i64::<()>()));
  let errs = lazy
    .decode_unknown_all(&Unknown::Array(vec![s("a"), Unknown::I64(1), s("b")]))
    .unwrap_err();
  let paths: Vec<_> = errs.issues.iter().map(|i| i.path.as_str()).collect();
  assert_eq!(paths, ["0", "2"]);
}

fn unknown_leaf() -> impl Strategy<Value = Unknown> {
  prop_oneof![
    Just(Unknown::Null),
    any::<bool>().prop_map(Unknown::Bool),
    (-5_i64..200).prop_map(Unknown::I64),
    "[a-z@.]{0,12}".prop_map(Unknown::String),
  ]
}

fn signup_input() -> impl Strategy<Value = Unknown> {
  prop::collection::btree_map(
    prop_oneof![Just("name"), Just("email"), Just("age"), Just("extra")],
    unknown_leaf(),
    0..4,
  )
  .prop_map(|m| Unknown::Object(m.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()))
}

proptest! {
  /// `decode_unknown_all` agrees with fail-fast `decode_unknown`.
  #[test]
  fn accumulating_agrees_with_fail_fast(input in signup_input()) {
    let fast = signup().decode_unknown(&input);
    let all = signup().decode_unknown_all(&input);
    match (fast, all) {
      (Ok(a), Ok(b)) => prop_assert_eq!(a, b),
      (Err(e), Err(es)) => {
        prop_assert!(!es.issues.is_empty());
        prop_assert_eq!(pair(&e), pair(&es.issues[0]));
      }
      (f, a) => prop_assert!(false, "disagree: {:?} vs {:?}", f.is_ok(), a.is_ok()),
    }
  }
}
