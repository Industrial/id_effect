//! Ex 042 — `yield_now` cooperatively yields (thread yield here).
use id_effect::{ThreadSleepRuntime, run_blocking, yield_now};

fn main() {
  let rt = ThreadSleepRuntime;
  assert_eq!(run_blocking(yield_now(&rt), ()), Ok(()));
  println!("042_yield_now ok");
}
