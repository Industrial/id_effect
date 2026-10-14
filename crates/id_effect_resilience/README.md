# id_effect_resilience

Resilience primitives for [`id_effect`](https://github.com/Industrial/id_effect) programs:

- **Circuit breaker** (`CircuitBreaker`) — fail fast after repeated errors; closed → open →
  half-open probe → closed
- **Retry through a breaker** (`retry_with_breaker`) — retry per a core `Schedule`, stop as soon
  as the breaker opens
- **Rate limiter** (`RateLimiter`) — token-bucket admission control
- **Bulkhead** (`Bulkhead`) — cap concurrent in-flight work
- **Hedged requests** (`hedged`) — race a delayed backup against a primary effect

All admission primitives are **fail-fast**: an open breaker, an empty token bucket or a full
bulkhead reject immediately with a typed error (`CircuitBreakerError`, `RateLimitError`,
`BulkheadError`) instead of waiting. In particular `Bulkhead` is built on the core `Semaphore` but
uses `try_acquire`: reach for `Semaphore` itself when callers should queue for a permit.

## Recipe: retry with backoff, guarded by a circuit breaker

`id_effect::retry` retries blindly until its `Schedule` is exhausted. During an outage that means
every caller sleeps through the full backoff and keeps hammering the dependency.
`retry_with_breaker` runs each attempt through `CircuitBreaker::call` — so outcomes are recorded
in the breaker — and stops with `Or::Left(CircuitBreakerError)` the moment the breaker is open.

```rust
use std::time::Duration;
use id_effect::failure::Or;
use id_effect::kernel::fail;
use id_effect::runtime::run_blocking;
use id_effect::scheduling::Schedule;
use id_effect_resilience::{CircuitBreaker, CircuitState, retry_with_breaker};

// Open after 3 consecutive failures; allow a half-open probe after 30 s.
let breaker = run_blocking(CircuitBreaker::make(3, Duration::from_secs(30)), ()).unwrap();

// Up to 10 retries, but the breaker cuts the loop short after the 3rd failure.
let out = run_blocking(
  retry_with_breaker(|| fail::<u32, &str, ()>("503"), Schedule::recurs(10), &breaker),
  (),
);
assert!(matches!(out, Err(Or::Left(_))));
assert_eq!(run_blocking(breaker.state(), ()).unwrap(), CircuitState::Open);
```

Outcomes: `Ok(a)` when an attempt succeeds, `Err(Or::Right(e))` with the last error when the
schedule is exhausted, `Err(Or::Left(CircuitBreakerError))` when the breaker is (or becomes) open.

### Deterministic tests

`CircuitBreaker::make_with_clock` and `RateLimiter::make_with_clock` read time from any
`id_effect::scheduling::Clock`, and `retry_with_breaker_and_clock` sleeps its backoff on one. With a
`TestClock`, backoff is virtual and the breaker cooldown advances only when you call
`clock.advance(..)` — no real sleeps, no flakiness.

## Examples

| Example | Shows |
|---------|-------|
| `010_retry_with_breaker` | transient failure → retry succeeds; outage → breaker opens and stops the retries; cooldown → half-open probe closes it |
| `020_rate_limit` | token-bucket burst and refill; a full bulkhead rejecting a concurrent call |

Run one with `cargo run -p id_effect_resilience --example 010_retry_with_breaker`.

See also the mdBook chapter *Runtime resilience* (`part5/ch21-00-runtime-resilience.md`).
