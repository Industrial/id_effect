# Retry with `TestClock`

**Problem.** A downstream call fails now and then. You want to retry it with exponential backoff,
and you want a test that checks the backoff timing without sleeping or depending on wall-clock
time.

**Recipe.** Describe the policy as a `Schedule`, run the call through `retry_with_clock`, and pass a
`TestClock`. `TestClock::sleep` records the deadline it was asked for and returns at once, so the
retry loop runs instantly. `pending_sleeps()` then shows exactly which delays were requested.

```rust
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use id_effect::{Effect, Schedule, TestClock, retry_with_clock, run_blocking};

/// Answers `503` for its first `failures` attempts, then `200 OK`.
fn flaky(calls: &Arc<AtomicU32>, failures: u32) -> Effect<&'static str, &'static str, ()> {
  let calls = Arc::clone(calls);
  Effect::new(move |_r: &mut ()| {
    if calls.fetch_add(1, Ordering::SeqCst) < failures { Err("503") } else { Ok("200 OK") }
  })
}

/// Exponential backoff from 100 ms, at most 4 retries.
fn backoff() -> Schedule {
  Schedule::exponential(Duration::from_millis(100)).compose(Schedule::recurs(4))
}

let start = Instant::now();
let clock = TestClock::new(start);
let calls = Arc::new(AtomicU32::new(0));
let c = Arc::clone(&calls);

let out = run_blocking(
  retry_with_clock(move || flaky(&c, 3), backoff(), clock.clone(), None),
  (),
);

assert_eq!(out, Ok("200 OK"));
assert_eq!(calls.load(Ordering::SeqCst), 4);
let waited: Vec<Duration> = clock.pending_sleeps().into_iter().map(|d| d - start).collect();
assert_eq!(waited, [100, 200, 400].map(Duration::from_millis));
```

When the schedule runs out, `retry_with_clock` returns the last error (`Err("503")` after five
attempts). Virtual time moves only when the test calls `clock.advance(…)`.

In production, pass a live clock (`LiveClock::new(runtime)`), or call `retry(make, schedule)`, which
uses one for you. The schedule and the effect stay the same.

Full example: [`crates/id_effect/examples/150_cookbook_retry_test_clock.rs`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/examples/150_cookbook_retry_test_clock.rs)

```bash
cargo run -p id_effect --example 150_cookbook_retry_test_clock
```

## Variant: retry behind a circuit breaker

Retrying a dependency that is down makes the outage worse. `id_effect_resilience::retry_with_breaker`
retries on the same `Schedule` but checks a `CircuitBreaker` before each attempt. Once the breaker
opens, the loop stops early even if the schedule would allow more attempts. Inject the
`TestClock` into both the breaker and the retry so that backoff and cooldown are virtual:

```rust
use id_effect_resilience::{CircuitBreaker, CircuitState, retry_with_breaker_and_clock};

let clock = TestClock::new(Instant::now());
// Opens after 3 consecutive failures, half-opens after a 30 s cooldown.
let breaker = run_blocking(
  CircuitBreaker::make_with_clock(3, Duration::from_secs(30), clock.clone()),
  (),
).expect("breaker");

let out = run_blocking(
  retry_with_breaker_and_clock(move || downstream_that_is_down(), backoff(), &breaker, clock.clone()),
  (),
);
assert!(out.is_err());
assert_eq!(run_blocking(breaker.state(), ()).unwrap(), CircuitState::Open);

clock.advance(Duration::from_secs(30)); // cooldown elapsed: the next attempt is a probe
```

The error type is `Or<CircuitBreakerError, E>`: `Or::Left` means the breaker refused the call
(or a breaker-level failure), and `Or::Right` is the call's own last error.

Full example: [`crates/id_effect_resilience/examples/010_retry_with_breaker.rs`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect_resilience/examples/010_retry_with_breaker.rs)

```bash
cargo run -p id_effect_resilience --example 010_retry_with_breaker
```

See also: [Schedule](../part3/ch11-01-schedule.md), [TestClock](../part4/ch15-02-test-clock.md),
[Runtime resilience](../part5/ch21-00-runtime-resilience.md).
