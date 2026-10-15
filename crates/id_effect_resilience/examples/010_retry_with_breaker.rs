//! Ex 010 — retry a flaky call per a `Schedule`, guarded by a `CircuitBreaker`.
//!
//! Delays are a few milliseconds of real time here; on a `TestExecutor` the same program runs on
//! virtual time (see the crate's tests).
//!
//! Run: `cargo run -p id_effect_resilience --example 010_retry_with_breaker`

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use id_effect::{Cause, Exit, Or};
use id_effect::{Effect, Schedule, fail, sleep, succeed, suspend};
use id_effect_resilience::{CircuitBreaker, CircuitBreakerError, CircuitState, retry_with_breaker};

type Call = Effect<&'static str, &'static str>;
type Guarded = Effect<&'static str, Or<CircuitBreakerError, &'static str>>;

/// A downstream call that fails (503) for its first `healthy_after` attempts, then succeeds.
fn downstream(
  calls: &Arc<AtomicU32>,
  healthy_after: u32,
) -> impl Fn() -> Call + Send + Sync + use<> {
  let calls = Arc::clone(calls);
  move || {
    let calls = Arc::clone(&calls);
    suspend(move || {
      let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
      if n > healthy_after {
        succeed("200 OK")
      } else {
        fail("503")
      }
    })
  }
}

/// Exponential backoff (1 ms, 2 ms, …), at most 5 retries.
fn backoff() -> Schedule<(Duration, u64), &'static str> {
  Schedule::exponential(Duration::from_millis(1)).intersect(Schedule::recurs(5))
}

fn run(eff: Guarded) -> Exit<&'static str, Or<CircuitBreakerError, &'static str>> {
  eff.block_on()
}

fn state(breaker: &CircuitBreaker) -> CircuitState {
  match breaker.state::<(), _>().block_on() {
    Exit::Success(s) => s,
    Exit::Failure(c) => panic!("state: {c:?}"),
  }
}

fn main() {
  let cooldown = Duration::from_millis(20);
  let breaker = CircuitBreaker::new(3, cooldown);
  let calls = Arc::new(AtomicU32::new(0));

  // 1. Transient failure: two 503s, then success — retried after 1 ms and 2 ms.
  let out = run(retry_with_breaker(
    downstream(&calls, 2),
    backoff(),
    &breaker,
  ));
  println!(
    "transient: {out:?} after {} calls",
    calls.load(Ordering::SeqCst)
  );
  assert!(matches!(out, Exit::Success("200 OK")));

  // 2. Outage: the breaker opens after 3 consecutive failures and the retry loop stops early,
  //    even though the schedule would allow more attempts.
  calls.store(0, Ordering::SeqCst);
  let out = run(retry_with_breaker(
    downstream(&calls, u32::MAX),
    backoff(),
    &breaker,
  ));
  println!(
    "outage: {out:?} after {} calls, breaker {:?}",
    calls.load(Ordering::SeqCst),
    state(&breaker)
  );
  assert!(matches!(
    out,
    Exit::Failure(Cause::Fail(Or::Left(CircuitBreakerError)))
  ));
  assert_eq!(state(&breaker), CircuitState::Open);

  // 3. Recovery: after the cooldown the next attempt is a half-open probe; success closes it.
  calls.store(0, Ordering::SeqCst);
  let out = run(sleep(cooldown).then(retry_with_breaker(
    downstream(&calls, 0),
    backoff(),
    &breaker,
  )));
  println!("recovered: {out:?}, breaker {:?}", state(&breaker));
  assert_eq!(state(&breaker), CircuitState::Closed);
}
