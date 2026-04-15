//! Ex 057 — `repeat_with_clock` uses an explicit clock for delays.
use id_effect::{Schedule, TestClock, repeat_with_clock, run_blocking, succeed};
use std::time::Instant;

fn main() {
  let clock = TestClock::new(Instant::now());
  let eff = repeat_with_clock(
    || succeed::<u32, (), ()>(1_u32),
    Schedule::recurs(1),
    clock,
    None,
  );
  let _ = run_blocking(eff, ());
  println!("057_repeat_with_clock ok");
}
