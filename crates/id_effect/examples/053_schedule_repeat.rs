//! Ex 053 — `repeat` runs while the schedule yields steps.
use id_effect::{Schedule, repeat, run_blocking, succeed};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

fn main() {
  let n = Arc::new(AtomicU32::new(0));
  let n2 = Arc::clone(&n);
  let eff = repeat(
    move || {
      n2.fetch_add(1, Ordering::SeqCst);
      succeed::<u32, (), ()>(1)
    },
    Schedule::recurs(2),
  );
  let _ = run_blocking(eff, ());
  assert_eq!(n.load(Ordering::SeqCst), 3);
  println!("053_schedule_repeat ok");
}
