//! Expansion behaviour of the kernel-neutral declarative macros (`pipe!`, `err!`) against the
//! real `::id_effect` paths. The v1 capability-DI macros are covered in `v1_di_macros.rs`.

use id_effect::Or;
use id_effect_macro::{err, pipe};

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
