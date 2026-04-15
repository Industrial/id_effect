//! Ex 040 — `Runtime` abstracts sleep / now / yield (here: `ThreadSleepRuntime`).
use id_effect::{Runtime, ThreadSleepRuntime, run_blocking};
use std::time::Duration;

fn main() {
  let rt = ThreadSleepRuntime;
  assert_eq!(run_blocking(rt.sleep(Duration::from_millis(0)), ()), Ok(()));
  println!("040_runtime_trait ok");
}
