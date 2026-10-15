//! Ex 055 — When retries are exhausted, the last error is the outcome.
use id_effect::{Cause, Exit};
use id_effect::{Effect, Schedule, fail};

fn main() {
  let eff: Effect<(), &'static str> = Effect::retry(|| fail("nope"), Schedule::recurs(1));
  assert_eq!(eff.block_on(), Exit::Failure(Cause::Fail("nope")));
  println!("055_schedule_exhausted ok");
}
