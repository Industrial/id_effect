//! Expansion behaviour of the declarative macros against the real `::id_effect` paths.

use id_effect::{Cap, CapList, Env, Or, build_env};
use id_effect_macro::{caps, err, mock_capability, pipe, provide, providers};

#[derive(Clone, Debug, PartialEq)]
struct Counter(u32);

#[derive(Clone, Debug, PartialEq)]
struct Name(&'static str);

mock_capability!(CounterMock, Counter, "counter/mock", || Counter(7));
mock_capability!(NameMock, Name, "name/mock", || Name("ada"));

fn inc(x: i32) -> i32 {
  x + 1
}

fn double(x: i32) -> i32 {
  x * 2
}

#[test]
fn pipe_with_only_a_value_is_identity() {
  assert_eq!(pipe!(5), 5);
}

#[test]
fn pipe_applies_functions_left_to_right() {
  assert_eq!(pipe!(3, inc, double), 8);
  assert_eq!(pipe!(3, double, inc), 7);
}

#[test]
fn pipe_accepts_closures_and_changes_types() {
  let out: Result<i32, _> = pipe!(" 42 ", str::trim, |s: &str| s.parse::<i32>());
  assert_eq!(out, Ok(42));
}

fn no_error() -> err!() {}

#[test]
fn err_with_no_types_is_unit() {
  // Coercing to `fn()` only type-checks if `err!()` expanded to `()`.
  let f: fn() = no_error;
  f();
  assert_eq!(std::mem::size_of::<err!()>(), 0);
}

#[test]
fn err_with_one_type_is_that_type() {
  let e: err!(u8) = 3u8;
  assert_eq!(e, 3);
}

#[test]
fn err_union_nests_or_right_associatively() {
  let first: err!(u8 | String | bool) = Or::Left(1);
  let middle: err!(u8 | String | bool) = Or::Right(Or::Left("io".to_owned()));
  let last: Or<u8, Or<String, bool>> = Or::Right(Or::Right(true));
  assert!(matches!(first, Or::Left(1)));
  assert!(matches!(middle, Or::Right(Or::Left(ref s)) if s == "io"));
  let _same_type: err!(u8 | String | bool) = last;
}

#[test]
fn caps_with_no_keys_is_plain_env() {
  let env: caps!() = Env::new();
  assert!(env.is_empty());
}

#[test]
fn caps_expands_to_cap_list_of_service_slots() {
  let list: caps!(Counter, Name) = CapList::new(Env::new());
  let _same_type: CapList<(Cap<Counter>, Cap<Name>)> = list;
}

#[test]
fn provide_and_mock_capability_build_an_env() {
  let env = build_env([provide!(CounterMock)]).expect("env");
  assert_eq!(env.get::<Cap<Counter>>(), &Counter(7));
}

#[test]
fn providers_expands_to_provider_array() {
  let bundle = providers!(dev: [CounterMock, NameMock]);
  assert_eq!(bundle.len(), 2);
  let env = build_env(bundle).expect("env");
  assert_eq!(env.get::<Cap<Counter>>(), &Counter(7));
  assert_eq!(env.get::<Cap<Name>>(), &Name("ada"));
}

#[test]
fn providers_accepts_trailing_comma_and_empty_bundle() {
  let one = providers!(single: [NameMock,]);
  assert_eq!(one.len(), 1);
  let none: [id_effect::ProviderBox; 0] = providers!(empty: []);
  assert!(build_env(none).expect("empty env").is_empty());
}
