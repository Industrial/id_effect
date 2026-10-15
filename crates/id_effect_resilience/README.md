# id_effect_resilience

Resilience primitives for [`id_effect`](https://github.com/Industrial/id_effect) programs, on the
0.7 kernel:

- **Circuit breaker** (`CircuitBreaker`) — fail fast after repeated errors; closed → open →
  half-open probe → closed
- **Retry through a breaker** (`retry_with_breaker`) — `Effect::retry` per a `Schedule`, stopping
  as soon as the breaker opens
- **Rate limiter** (`RateLimiter`) — token-bucket admission control
- **Bulkhead** (`Bulkhead`) — cap concurrent in-flight work (on `Semaphore`)
- **Hedged requests** (`hedged`) — `Effect::race` a primary against a delayed backup

Constructors are plain functions (`CircuitBreaker::new`, `RateLimiter::new`, `Bulkhead::new`),
and every operation returns a lazy `Effect`, generic over its environment.

All admission primitives are **fail-fast**: an open breaker, an empty token bucket or a full
bulkhead reject immediately with a typed error (`CircuitBreakerError`, `RateLimitError`,
`BulkheadError`) instead of waiting. Reach for `Semaphore::with_permit` when callers should
queue for a permit.

## Recipe: retry with backoff, guarded by a circuit breaker

`Effect::retry` retries blindly until its `Schedule` is exhausted. During an outage that means
every caller sleeps through the full backoff and keeps hammering the dependency.
`retry_with_breaker` runs each attempt through `CircuitBreaker::call` — so outcomes are recorded
in the breaker — and stops with `Or::Left(CircuitBreakerError)` the moment the breaker is open.

```rust
use std::time::Duration;

use id_effect::{Cause, Exit, Or};
use id_effect::{Effect, Schedule, fail};
use id_effect_resilience::{CircuitBreaker, CircuitState, retry_with_breaker};

// Open after 3 consecutive failures; allow a half-open probe after 30 s.
let breaker = CircuitBreaker::new(3, Duration::from_secs(30));

// Up to 10 retries, but the breaker cuts the loop short after the 3rd failure.
// Effects are one-shot, so the attempt is a factory called once per attempt.
let eff: Effect<u32, _> = retry_with_breaker(|| fail("503"), Schedule::recurs(10), &breaker);
assert!(matches!(eff.block_on(), Exit::Failure(Cause::Fail(Or::Left(_)))));
assert_eq!(breaker.state::<(), _>().block_on(), Exit::Success(CircuitState::Open));
```

Outcomes: success when an attempt succeeds, `Or::Right(e)` with the last error when the schedule
is exhausted, `Or::Left(CircuitBreakerError)` when the breaker is (or becomes) open.

### Deterministic tests

Time — the breaker cooldown, token refill, retry backoff and hedge delay — is read from the
running fiber's executor clock. On a `TestExecutor` that is virtual time: a program that
`sleep`s past the cooldown sees the breaker half-open at once, with no real sleeps.

## Examples

| Example | Shows |
|---------|-------|
| `010_retry_with_breaker` | transient failure → retry succeeds; outage → breaker opens and stops the retries; cooldown → half-open probe closes it |
| `020_rate_limit` | token-bucket burst and refill; a full bulkhead rejecting a concurrent call; a hedged call |

Run one with `cargo run -p id_effect_resilience --example 010_retry_with_breaker`.

See also the mdBook chapter *Runtime resilience* (`part5/ch21-00-runtime-resilience.md`).
