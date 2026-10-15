//! Ex 018 — `Exit` models a completed run (`Success` / `Failure(Cause)`).
use id_effect::{Cause, Exit};
use id_effect::{Effect, die, fail, succeed};

fn main() {
  let ok: Effect<i32, ()> = succeed(42);
  assert_eq!(ok.block_on(), Exit::succeed(42));
  let failed: Effect<i32, &'static str> = fail("no");
  assert_eq!(failed.block_on(), Exit::fail("no"));
  let died: Effect<i32, &'static str> = die("boom");
  assert!(matches!(died.block_on(), Exit::Failure(Cause::Die(_))));
  println!("018_exit_type ok");
}
