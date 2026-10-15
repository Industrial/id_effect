//! Ex 015 — `Or::Left` / `Or::Right` tag the failing branch of an `err!` union.
use id_effect::next::{Effect, fail};
use id_effect::{Cause, Exit, Or, err};

#[derive(Debug, Clone, PartialEq, Eq)]
enum L {
  A,
}
#[derive(Debug, Clone, PartialEq, Eq)]
enum R {
  B,
}

fn main() {
  let l: Effect<(), err!(L | R)> = fail(L::A).map_err(Or::Left);
  assert!(matches!(
    l.block_on(),
    Exit::Failure(Cause::Fail(Or::Left(L::A)))
  ));
  let r: Effect<(), err!(L | R)> = fail(R::B).map_err(Or::Right);
  assert!(matches!(
    r.block_on(),
    Exit::Failure(Cause::Fail(Or::Right(R::B)))
  ));
  println!("015_or_variants ok");
}
