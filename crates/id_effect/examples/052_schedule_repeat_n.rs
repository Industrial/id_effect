//! Ex 052 — `Effect::repeat` with `Schedule::recurs(n)` runs the effect once, then `n` more
//! times; the outcome is the schedule's output (the repeat count).
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use id_effect::Exit;
use id_effect::next::{Effect, Schedule, sync};

fn main() {
  let calls = Arc::new(AtomicU32::new(0));
  let c = Arc::clone(&calls);
  let eff: Effect<u64, ()> = Effect::repeat(
    move || {
      let c = Arc::clone(&c);
      sync(move || c.fetch_add(1, Ordering::SeqCst) + 1)
    },
    Schedule::recurs(2),
  );
  assert_eq!(eff.block_on(), Exit::succeed(2));
  assert_eq!(calls.load(Ordering::SeqCst), 3);
  println!("052_schedule_repeat_n ok");
}
