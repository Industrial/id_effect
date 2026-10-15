# `id_effect_resilience`: on the 0.7 kernel

0.7 moves `id_effect_resilience` to the new kernel. Every operation returns
an `Effect`, generic over its environment `R`. Constructors are plain functions now (they
were effects), and all time — the breaker cooldown, token refill, retry backoff and the hedge
delay — is read from the running fiber's executor clock, so it is virtual time on
`TestExecutor`. The `*_with_clock` variants are gone.

| item | 0.6 | 0.7 |
|---|---|---|
| `CircuitBreaker` | `CircuitBreaker::make(n, reset)` (an effect), `make_with_clock(.., clock)` | `CircuitBreaker::new(n, reset)` (plain) |
| `CircuitBreaker::call` / `state` | v1 `Effect<A, Or<CircuitBreakerError, E>, R>` / `Effect<CircuitState>` | `Effect<A, Or<CircuitBreakerError, E>, R>` / `Effect<CircuitState, E, R>` |
| `RateLimiter` | `RateLimiter::make(rate, cap)` (an effect), `make_with_clock` | `RateLimiter::new(rate, cap)` (plain); `acquire::<R>()` |
| `Bulkhead` | `Bulkhead::make(n)` (an effect) | `Bulkhead::new(n)` (plain), on `Semaphore`; `available()` added |
| `retry_with_breaker` | `(FnMut() -> Effect, v1 Schedule, &breaker)`; `retry_with_breaker_and_clock` | `(Fn() -> Effect + Send + Sync, Schedule<Out, E>, &breaker)`, built on `Effect::retry`; the `_and_clock` variant is gone |
| `hedged` | `tokio::select!`, `R = ()` only: the first *completion* wins | `primary.race(sleep(delay).then(backup))`, any `R`: the first *success* wins |

```rust
// 0.6
let breaker = run_blocking(CircuitBreaker::make(3, Duration::from_secs(30)), ()).unwrap();
let out = run_blocking(
  retry_with_breaker(|| call(), Schedule::exponential(ms(100)).compose(Schedule::recurs(5)), &breaker),
  (),
);
let bh = run_blocking(Bulkhead::make(8), ()).unwrap();

// 0.7
use id_effect::{Effect, Schedule};
let breaker = CircuitBreaker::new(3, Duration::from_secs(30));
let out = retry_with_breaker(
  || call(), // a factory: effects are one-shot, so each attempt builds a fresh one
  Schedule::exponential(ms(100)).intersect(Schedule::recurs(5)),
  &breaker,
)
.block_on();
let bh = Bulkhead::new(8);
```

- **Tests on virtual time.** Replace `TestClock` + `make_with_clock` with a program that sleeps
  on a `TestExecutor`: `cb.call(fail(..)).then(sleep(reset)).then(cb.call(probe))` sees the
  breaker half-open after `reset` of virtual time, and backoff delays advance the same clock.
- **`hedged` semantics changed.** It is `Effect::race`: a primary that *fails* before the delay
  no longer ends the hedge — the backup still starts at the delay and decides. The loser is
  interrupted and awaited (its finalizers have run). When both fail, the cause is
  `Cause::Both(primary, backup)`. The backup is never started if the primary succeeds first.
- **Defects and interrupts** pass through `CircuitBreaker::call` without being counted, and are
  never retried by `retry_with_breaker`. `Bulkhead::with_permit` releases its permit however
  the effect ends, interrupts included.
- `RateLimiter` counts refill from its first `acquire` (the bucket starts full).
- **An unmigrated (v1) caller** wraps the effect with the bridge:
  `use id_effect::compat::IntoV1; cb.call(eff).into_v1::<R>()`.
