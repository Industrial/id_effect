//! Ex 015 — `Or::Left` / `Or::Right` tag the failing branch.
use id_effect::{Effect, Or, fail, run_blocking};

#[derive(Debug, Clone, PartialEq, Eq)]
enum L {
  A,
}
#[derive(Debug, Clone, PartialEq, Eq)]
enum R {
  B,
}

fn main() {
  let l: Effect<(), Or<L, R>, ()> = fail(L::A).union_error::<R>();
  assert_eq!(run_blocking(l, ()), Err(Or::Left(L::A)));
  let r: Effect<(), Or<L, R>, ()> = fail(R::B).map_error(Or::Right);
  assert_eq!(run_blocking(r, ()), Err(Or::Right(R::B)));
  println!("015_or_variants ok");
}
