//! Ex 041 — `ThreadSleepRuntime` blocks the OS thread for `sleep`.
use id_effect::{Runtime, ThreadSleepRuntime, run_blocking};
use std::time::Duration;

fn main() {
  let rt = ThreadSleepRuntime;
  let t0 = std::time::Instant::now();
  let _ = run_blocking(rt.sleep(Duration::from_millis(1)), ());
  assert!(t0.elapsed() >= Duration::from_millis(1));
  println!("041_thread_sleep_runtime ok");
}
