//! Ex 052 — `repeat_n` runs the factory once, then repeats while [`Schedule::recurs`] has budget.
//!
//! With `times = n`, you get **n + 1** invocations (initial run plus `n` repeats); the returned
//! value is from the last successful run.
use id_effect::{repeat_n, run_blocking, succeed};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

fn main() {
  let calls = Arc::new(AtomicU32::new(0));
  let calls_c = Arc::clone(&calls);
  let eff = repeat_n(
    move || {
      let c = Arc::clone(&calls_c);
      succeed::<u32, (), ()>(c.fetch_add(1, Ordering::SeqCst) + 1)
    },
    2,
  );
  let last = run_blocking(eff, ()).expect("repeat_n");
  assert_eq!(last, 3);
  assert_eq!(calls.load(Ordering::SeqCst), 3);
  println!("052_schedule_repeat_n ok");
}
