//! Ex 098 — `run_test_with_clock` accepts an explicit `TestClock` (deterministic time source).
use id_effect::{Exit, TestClock, run_test_with_clock, succeed};
use std::time::Instant;

fn main() {
  let clock = TestClock::new(Instant::now());
  assert_eq!(
    run_test_with_clock(succeed::<u8, (), ()>(8_u8), (), clock),
    Exit::succeed(8)
  );
  println!("098_run_test_clock_hygiene ok");
}
