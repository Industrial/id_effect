//! Ex 058 — `retry_with_clock` retries with clock-aware spacing.
use id_effect::{Schedule, TestClock, fail, retry_with_clock, run_blocking, succeed};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

fn main() {
  let clock = TestClock::new(Instant::now());
  let n = Arc::new(AtomicU32::new(0));
  let n2 = Arc::clone(&n);
  let eff = retry_with_clock(
    move || {
      if n2.fetch_add(1, Ordering::SeqCst) == 0 {
        fail::<u8, &'static str, ()>("once")
      } else {
        succeed(7_u8)
      }
    },
    Schedule::recurs(3),
    clock,
    None,
  );
  assert_eq!(run_blocking(eff, ()), Ok(7));
  println!("058_retry_with_clock ok");
}
