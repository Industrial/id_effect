//! Ex 055 — When retries are exhausted, the last error is returned.
use id_effect::{Schedule, fail, retry, run_blocking};

fn main() {
  let eff = retry(|| fail::<(), &'static str, ()>("nope"), Schedule::recurs(1));
  assert_eq!(run_blocking(eff, ()), Err("nope"));
  println!("055_schedule_exhausted ok");
}
