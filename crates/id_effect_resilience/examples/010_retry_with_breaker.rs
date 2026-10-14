//! Ex 010 — retry a flaky call per a `Schedule`, guarded by a `CircuitBreaker`.
//!
//! Uses a `TestClock`, so backoff and the breaker cooldown are virtual: the example runs instantly
//! and prints the same output every time.
//!
//! Run: `cargo run -p id_effect_resilience --example 010_retry_with_breaker`

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use id_effect::failure::Or;
use id_effect::kernel::Effect;
use id_effect::runtime::run_blocking;
use id_effect::scheduling::{Schedule, TestClock};
use id_effect_resilience::{CircuitBreaker, CircuitState, retry_with_breaker_and_clock};

/// A downstream call that fails (503) for its first `healthy_after` attempts, then succeeds.
fn downstream(
  calls: &Arc<AtomicU32>,
  healthy_after: u32,
) -> Effect<&'static str, &'static str, ()> {
  let calls = Arc::clone(calls);
  Effect::new(move |_r: &mut ()| {
    let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
    if n > healthy_after {
      Ok("200 OK")
    } else {
      Err("503")
    }
  })
}

fn main() {
  let clock = TestClock::new(Instant::now());
  let breaker = run_blocking(
    CircuitBreaker::make_with_clock(3, Duration::from_secs(30), clock.clone()),
    (),
  )
  .expect("breaker");
  let calls = Arc::new(AtomicU32::new(0));
  // Exponential backoff (100 ms, 200 ms, …), at most 5 retries.
  let backoff = || Schedule::exponential(Duration::from_millis(100)).compose(Schedule::recurs(5));

  // 1. Transient failure: two 503s, then success — retried with 100 ms, 200 ms backoff.
  let c = Arc::clone(&calls);
  let out = run_blocking(
    retry_with_breaker_and_clock(
      move || downstream(&c, 2),
      backoff(),
      &breaker,
      clock.clone(),
    ),
    (),
  );
  println!(
    "transient: {out:?} after {} calls",
    calls.load(Ordering::SeqCst)
  );
  assert_eq!(out.ok(), Some("200 OK"));

  // 2. Outage: the breaker opens after 3 consecutive failures and the retry loop stops early,
  //    even though the schedule would allow more attempts.
  calls.store(0, Ordering::SeqCst);
  let c = Arc::clone(&calls);
  let out = run_blocking(
    retry_with_breaker_and_clock(
      move || downstream(&c, u32::MAX),
      backoff(),
      &breaker,
      clock.clone(),
    ),
    (),
  );
  let state = run_blocking(breaker.state(), ()).expect("state");
  println!(
    "outage: {out:?} after {} calls, breaker {state:?}",
    calls.load(Ordering::SeqCst)
  );
  assert!(matches!(out, Err(Or::Left(_))));
  assert_eq!(state, CircuitState::Open);

  // 3. Recovery: after the cooldown the next attempt is a half-open probe; success closes it.
  clock.advance(Duration::from_secs(30));
  calls.store(0, Ordering::SeqCst);
  let c = Arc::clone(&calls);
  let out = run_blocking(
    retry_with_breaker_and_clock(
      move || downstream(&c, 0),
      backoff(),
      &breaker,
      clock.clone(),
    ),
    (),
  );
  let state = run_blocking(breaker.state(), ()).expect("state");
  println!("recovered: {out:?}, breaker {state:?}");
  assert_eq!(state, CircuitState::Closed);
}
