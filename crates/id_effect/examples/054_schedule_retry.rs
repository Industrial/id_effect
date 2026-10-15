//! Ex 054 — `Effect::retry` re-runs on failure until it succeeds or the schedule is exhausted.
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use id_effect::Exit;
use id_effect::{Effect, Schedule, fail, succeed, suspend};

fn main() {
  let n = Arc::new(AtomicU32::new(0));
  let n2 = Arc::clone(&n);
  let eff: Effect<u8, &'static str> = Effect::retry(
    move || {
      let n = Arc::clone(&n2);
      suspend(move || {
        if n.fetch_add(1, Ordering::SeqCst) < 2 {
          fail("retry me")
        } else {
          succeed(42)
        }
      })
    },
    Schedule::recurs(5),
  );
  assert_eq!(eff.block_on(), Exit::succeed(42));
  assert_eq!(n.load(Ordering::SeqCst), 3);
  println!("054_schedule_retry ok");
}
