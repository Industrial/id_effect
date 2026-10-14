// (1) A capability read that is not in the effect's `R`.
use kernel_a::{Effect, Never, caps, effect};
use spike_corpus::{Db, Logger};

#[derive(Debug)]
pub struct Metrics;

fn handler() -> Effect<String, Never, caps!(Db, Logger)> {
  effect!({
    let _db = ~Db;
    let m = ~Metrics;
    format!("{m:?}")
  })
}

fn main() {
  let _ = handler();
}
