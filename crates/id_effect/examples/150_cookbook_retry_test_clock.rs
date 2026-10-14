//! Ex 150 — Cookbook: retry an effect on a `Schedule` and check its backoff with a `TestClock`.
//!
//! `TestClock::sleep` records the requested deadline and returns at once, so the retry loop runs
//! instantly and its timing is still observable through `pending_sleeps()`. No real sleeping, no
//! timing-dependent assertions. See the book chapter "Cookbook → Retry with TestClock".
//!
//! Run: `cargo run -p id_effect --example 150_cookbook_retry_test_clock`

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use id_effect::{Effect, Schedule, TestClock, retry_with_clock, run_blocking};

/// A downstream call that answers `503` for its first `failures` attempts, then `200 OK`.
fn flaky(calls: &Arc<AtomicU32>, failures: u32) -> Effect<&'static str, &'static str, ()> {
  let calls = Arc::clone(calls);
  Effect::new(move |_r: &mut ()| {
    if calls.fetch_add(1, Ordering::SeqCst) < failures {
      Err("503")
    } else {
      Ok("200 OK")
    }
  })
}

/// Exponential backoff from 100 ms, at most 4 retries (5 attempts in total).
fn backoff() -> Schedule {
  Schedule::exponential(Duration::from_millis(100)).compose(Schedule::recurs(4))
}

/// The delays the retry loop asked the clock for, relative to `start`.
fn delays(clock: &TestClock, start: Instant) -> Vec<Duration> {
  clock
    .pending_sleeps()
    .into_iter()
    .map(|deadline| deadline - start)
    .collect()
}

fn main() {
  let start = Instant::now();

  // 1. Transient failure: three 503s, then success.
  let clock = TestClock::new(start);
  let calls = Arc::new(AtomicU32::new(0));
  let c = Arc::clone(&calls);
  let out = run_blocking(
    retry_with_clock(move || flaky(&c, 3), backoff(), clock.clone(), None),
    (),
  );
  let waited = delays(&clock, start);
  println!(
    "transient: {out:?} after {} attempts, backoff {waited:?}",
    calls.load(Ordering::SeqCst)
  );
  assert_eq!(out, Ok("200 OK"));
  assert_eq!(calls.load(Ordering::SeqCst), 4);
  assert_eq!(
    waited,
    [100, 200, 400].map(Duration::from_millis),
    "one exponential delay per retry"
  );

  // 2. Outage: the schedule runs out and the last error is returned.
  let clock = TestClock::new(start);
  let calls = Arc::new(AtomicU32::new(0));
  let c = Arc::clone(&calls);
  let out = run_blocking(
    retry_with_clock(move || flaky(&c, u32::MAX), backoff(), clock.clone(), None),
    (),
  );
  println!(
    "outage: {out:?} after {} attempts, {} delays",
    calls.load(Ordering::SeqCst),
    clock.pending_sleeps().len()
  );
  assert_eq!(out, Err("503"));
  assert_eq!(calls.load(Ordering::SeqCst), 5);
  assert_eq!(clock.pending_sleeps().len(), 4);

  // 3. Virtual time only moves when the test says so.
  clock.advance(Duration::from_millis(250));
  assert_eq!(delays(&clock, start).len(), 2, "100 ms and 200 ms elapsed");
  println!("150_cookbook_retry_test_clock ok");
}
