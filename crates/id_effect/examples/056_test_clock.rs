//! Ex 056 — `TestClock` pairs with schedule helpers in tests.
use id_effect::{TestClock, run_blocking, succeed};
use std::time::Instant;

fn main() {
  let c = TestClock::new(Instant::now());
  assert_eq!(run_blocking(succeed::<u8, (), ()>(1_u8), ()), Ok(1));
  let _ = c;
  println!("056_test_clock ok");
}
