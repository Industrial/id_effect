//! Ex 002 — A typed failure surfaces at the program boundary as `Exit::Failure`.
use id_effect::next::{Effect, fail};
use id_effect::{Cause, Exit};

fn main() {
  let program: Effect<(), &'static str> = fail("not_ready");
  let exit = program.block_on();
  assert_eq!(exit, Exit::Failure(Cause::Fail("not_ready")));
  assert_eq!(exit.into_result(), Err(Cause::Fail("not_ready")));
  println!("002_fail_boundary ok");
}
