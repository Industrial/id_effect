//! Ex 059 — Cancellation short-circuits clock-driven repeat/retry.
use id_effect::{
  CancellationToken, Schedule, TestClock, fail, repeat_with_clock_and_interrupt,
  retry_with_clock_and_interrupt, run_blocking, succeed,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

fn main() {
  let clock = TestClock::new(Instant::now());
  let tok = CancellationToken::new();
  tok.cancel();
  let n = Arc::new(AtomicUsize::new(0));
  let n2 = Arc::clone(&n);
  let rep = repeat_with_clock_and_interrupt(
    move || {
      n2.fetch_add(1, Ordering::SeqCst);
      succeed::<usize, (), ()>(1)
    },
    Schedule::recurs(5),
    clock.clone(),
    tok.clone(),
    None,
  );
  assert_eq!(run_blocking(rep, ()), Ok(1));
  assert_eq!(n.load(Ordering::SeqCst), 1);

  let n3 = Arc::new(AtomicUsize::new(0));
  let n4 = Arc::clone(&n3);
  let ret = retry_with_clock_and_interrupt(
    move || {
      n4.fetch_add(1, Ordering::SeqCst);
      fail::<(), &'static str, ()>("x")
    },
    Schedule::recurs(5),
    clock,
    tok,
    None,
  );
  assert_eq!(run_blocking(ret, ()), Err("x"));
  assert_eq!(n3.load(Ordering::SeqCst), 1);
  println!("059_schedule_interrupt ok");
}
