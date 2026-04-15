//! Ex 054 — `retry` re-runs on failure until the schedule exhausts.
use id_effect::{Schedule, fail, retry, run_blocking, succeed};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

fn main() {
  let n = Arc::new(AtomicU32::new(0));
  let n2 = Arc::clone(&n);
  let eff = retry(
    move || {
      let c = n2.fetch_add(1, Ordering::SeqCst);
      if c < 2 {
        fail::<u8, &'static str, ()>("retry me")
      } else {
        succeed(42_u8)
      }
    },
    Schedule::recurs(5),
  );
  assert_eq!(run_blocking(eff, ()), Ok(42));
  println!("054_schedule_retry ok");
}
