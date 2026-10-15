// (1) A capability read that is not in the effect's `R`.
use id_effect::Never;
use id_effect::next::{Effect, caps_next, effect_next};

pub struct Db;
pub struct Logger;

#[derive(Debug)]
pub struct Metrics;

fn handler() -> Effect<String, Never, caps_next!(Db, Logger)> {
  effect_next!({
    let _db = ~Db;
    let m = ~Metrics;
    format!("{m:?}")
  })
}

fn main() {
  let _ = handler();
}
