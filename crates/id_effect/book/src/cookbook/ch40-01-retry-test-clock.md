# Retry with virtual time

**Problem.** A downstream call fails now and then. You want to retry it with exponential backoff,
and you want a test that checks the backoff timing without sleeping or depending on wall-clock
time.

**Recipe.** Describe the policy as a `Schedule` and retry with `Effect::retry`. Effects are
one-shot, so `retry` takes a *factory* that builds a fresh attempt each time. All delays are read
from the executor the fiber runs on, so the same program runs on real time in production and on
virtual time when a test runs it on a `TestExecutor`.

```rust
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use id_effect::{Effect, Schedule, fail, succeed, suspend};

/// Answers `503` for its first `failures` attempts, then `200 OK`.
fn flaky(calls: &Arc<AtomicU32>, failures: u32) -> impl Fn() -> Effect<&'static str, &'static str>
+ Send + Sync + use<> {
  let calls = Arc::clone(calls);
  move || {
    let calls = Arc::clone(&calls);
    suspend(move || {
      if calls.fetch_add(1, Ordering::SeqCst) < failures { fail("503") } else { succeed("200 OK") }
    })
  }
}

/// Exponential backoff from 100 ms, at most 4 retries.
fn backoff() -> Schedule<(Duration, u64), &'static str> {
  Schedule::exponential(Duration::from_millis(100)).intersect(Schedule::recurs(4))
}

let calls = Arc::new(AtomicU32::new(0));
let program = Effect::retry(flaky(&calls, 3), backoff());
# let _ = program;
```

In production, run it at the edge: `program.block_on()` waits 100 + 200 + 400 ms of real time.
In a test, run it on a `TestExecutor` with the `run_virtual` helper from
[Virtual Time with TestExecutor](../part4/ch15-02-test-clock.md), which advances the virtual clock
from timer to timer:

```rust
# use std::future::Future;
# use std::pin::pin;
# use std::sync::Arc;
# use std::sync::atomic::{AtomicU32, Ordering};
# use std::task::{Context, Poll, Waker};
# use std::time::Duration;
# use id_effect::{Effect, Executor, Exit, Schedule, TestExecutor, fail, succeed, suspend};
# fn flaky(calls: &Arc<AtomicU32>, failures: u32) -> impl Fn() -> Effect<&'static str, &'static str>
# + Send + Sync + use<> {
#   let calls = Arc::clone(calls);
#   move || {
#     let calls = Arc::clone(&calls);
#     suspend(move || {
#       if calls.fetch_add(1, Ordering::SeqCst) < failures { fail("503") } else { succeed("200 OK") }
#     })
#   }
# }
# fn backoff() -> Schedule<(Duration, u64), &'static str> {
#   Schedule::exponential(Duration::from_millis(100)).intersect(Schedule::recurs(4))
# }
# fn take_ready<F: Future>(fut: F) -> F::Output {
#   match pin!(fut).as_mut().poll(&mut Context::from_waker(Waker::noop())) {
#     Poll::Ready(v) => v,
#     Poll::Pending => panic!("root fiber is not done"),
#   }
# }
# fn run_virtual<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> (Exit<A, E>, Duration) {
#   let exec = TestExecutor::new();
#   let start = exec.now();
#   let h = eff.fork_root(Arc::new(exec.clone()));
#   exec.run_until_stalled();
#   while !h.is_done() {
#     let deadline = exec.next_timer_deadline().expect("stalled with no timer pending");
#     exec.advance(deadline.saturating_duration_since(exec.now()));
#   }
#   (take_ready(h.join_future()), exec.now() - start)
# }
// In a #[cfg(test)] module, under #[test]:
fn retries_back_off_exponentially() {
  let calls = Arc::new(AtomicU32::new(0));
  let (exit, took) = run_virtual(Effect::retry(flaky(&calls, 3), backoff()));
  assert_eq!(exit, Exit::succeed("200 OK"));
  assert_eq!(calls.load(Ordering::SeqCst), 4);
  assert_eq!(took, Duration::from_millis(100 + 200 + 400)); // virtual, not wall-clock
}
# retries_back_off_exponentially();
```

When the schedule runs out, `retry` fails with the last error (`"503"` after five attempts).
`Effect::retry_or_else(make, schedule, fallback)` handles exhaustion instead, and
`Effect::repeat` / `repeat_or_else` drive successful runs on a schedule. The kernel's
`crates/id_effect/tests/schedule.rs` checks these timings to the nanosecond the same way.

## Variant: retry behind a circuit breaker

Retrying a dependency that is down makes the outage worse.
`id_effect_resilience::retry_with_breaker` retries on the same `Schedule` but checks a
`CircuitBreaker` before each attempt. Once the breaker opens, the loop stops early even if the
schedule would allow more attempts. The breaker's cooldown also reads the executor clock, so it is
virtual on a `TestExecutor` too.

```rust
# use std::sync::Arc;
# use std::sync::atomic::{AtomicU32, Ordering};
# use std::time::Duration;
# use id_effect::{Cause, Effect, Exit, Or, Schedule, fail, sleep, succeed, suspend};
# fn flaky(calls: &Arc<AtomicU32>, failures: u32) -> impl Fn() -> Effect<&'static str, &'static str>
# + Send + Sync + use<> {
#   let calls = Arc::clone(calls);
#   move || {
#     let calls = Arc::clone(&calls);
#     suspend(move || {
#       if calls.fetch_add(1, Ordering::SeqCst) < failures { fail("503") } else { succeed("200 OK") }
#     })
#   }
# }
# fn backoff() -> Schedule<(Duration, u64), &'static str> {
#   Schedule::exponential(Duration::from_millis(100)).intersect(Schedule::recurs(4))
# }
# let calls = Arc::new(AtomicU32::new(0));
use id_effect_resilience::{CircuitBreaker, CircuitBreakerError, CircuitState, retry_with_breaker};

// Opens after 3 consecutive failures, half-opens after the cooldown. Keep the cooldown longer
// than the backoff delays: an attempt that starts after the cooldown is a half-open probe, not a
// refusal.
let breaker = CircuitBreaker::new(3, Duration::from_secs(1));

let out = retry_with_breaker(flaky(&calls, u32::MAX), backoff(), &breaker).block_on();
assert!(matches!(out, Exit::Failure(Cause::Fail(Or::Left(CircuitBreakerError)))));
assert_eq!(breaker.state::<(), _>().block_on(), Exit::succeed(CircuitState::Open));

// After the cooldown the next attempt is a half-open probe; a success closes the breaker.
let out = sleep(Duration::from_secs(1))
  .then(retry_with_breaker(flaky(&calls, 0), backoff(), &breaker))
  .block_on();
# assert!(matches!(out, Exit::Success("200 OK")));
```

The error type is `Or<CircuitBreakerError, E>`: `Or::Left` means the breaker refused the call, and
`Or::Right` is the call's own last error. Defects and interrupts are neither counted by the breaker
nor retried.

Full example: [`crates/id_effect_resilience/examples/010_retry_with_breaker.rs`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect_resilience/examples/010_retry_with_breaker.rs)

```bash
cargo run -p id_effect_resilience --example 010_retry_with_breaker
```

See also: [Schedule](../part3/ch11-01-schedule.md),
[Virtual Time with TestExecutor](../part4/ch15-02-test-clock.md),
[Runtime resilience](../part5/ch21-00-runtime-resilience.md).
