// Gallery: `~fallible()` inside an infallible (`Never`) effect.
// Fix: handle the error first, `~load().catch(|_| succeed(0))`, or widen the body's error type.
use id_effect::Never;
use id_effect::{Effect, effect, fail};

pub struct DbError;

fn load() -> Effect<u32, DbError> {
  fail(DbError)
}

fn handler() -> Effect<u32, Never> {
  effect!({ ~load() + 1 })
}

fn main() {
  let _ = handler();
}
