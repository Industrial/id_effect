# Runtime Resilience

Production services need more than happy-path effects. Timeouts, overload and flaky dependencies are normal. This chapter covers a few core primitives and the `id_effect_resilience` crate for keeping programs responsive under stress.

## What This Chapter Covers

- **[`RequestResolver`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/request.rs)** — batch concurrent lookups into one fetch
- **[`SubscriptionRef`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/coordination/subscription_ref.rs)** — shared state plus change notifications
- **[`Redacted`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/schema/redacted.rs)** — schema-layer secrets with masked `Debug`
- **`match_effect!`** — enum match helper with path-prefixed arms
- **`id_effect_resilience`** — circuit breaker, rate limiter, bulkhead, hedged requests

## Request batching

Effect.ts batches data-source lookups so N concurrent `getUser(id)` calls become one SQL `WHERE id IN (…)`. In id_effect a [`Request`] is a value that names its `Output` and `Error`; a [`RequestResolver::make_batched`] answers a whole batch of distinct requests with one effect, returning one `Result` per request in order. Requests made together (for example by the children of one `all`) are deduplicated and resolved in a single call:

```rust
use id_effect::{Concurrency, Effect, Exit, Request, RequestResolver, all, sync};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct GetUser(u32);

impl Request for GetUser {
    type Output = String;
    type Error = String;
}

let users = RequestResolver::make_batched(|reqs: Vec<GetUser>| {
    sync(move || reqs.into_iter().map(|GetUser(id)| Ok(format!("user-{id}"))).collect())
});

// Three requests started together: one resolver call with the keys {1, 2}.
let names = all(
    [1, 2, 1].map(|id| Effect::request(GetUser(id), &users)),
    Concurrency::Unbounded,
);
assert!(matches!(names.block_on(), Exit::Success(v) if v == ["user-1", "user-2", "user-1"]));
```

`with_request_cache` / `RequestCache::layer` add a request cache scoped to a `Scope` or a `Layer`.

## SubscriptionRef

[`SubscriptionRef`] combines [`Ref`] with [`PubSub`]. Every `set` / `update` / `modify` publishes the new value; `subscribe(&scope)` returns a [`Queue`] that receives the current value first, then every later change, until the scope closes:

```rust
use id_effect::{Effect, SubscriptionRef};

let cell = SubscriptionRef::new(0u32);
let program: Effect<()> = Effect::scoped(move |scope| {
    cell.subscribe(&scope)
        .flat_map(move |_changes| cell.set(1)) // `_changes` sees 0, then 1
});
```

## Redacted values

Use [`Redacted<T>`] anywhere a schema or domain type might reach logs. `Debug` and `Display` print `<redacted>`; call `expose()` only at trust boundaries.

## match_effect!

The `match_effect!` proc macro prefixes variant names so the compiler checks exhaustiveness without repeating the enum path:

```rust
use id_effect::match_effect;

enum Shape { Circle(f64), Rect { w: f64, h: f64 } }

let shape = Shape::Rect { w: 2.0, h: 4.0 };
let area = match_effect!(Shape, shape, {
    Circle(r) => 3.14 * r * r,
    Rect { w, h } => w * h,
});
assert_eq!(area, 8.0);
```

## id_effect_resilience

Add `id_effect_resilience` when you need operational guardrails. Constructors are plain functions; every operation returns an `Effect`, generic over its environment, and all time (cooldowns, refill, backoff, hedge delay) is read from the fiber's executor clock, so it runs on virtual time under `TestExecutor`.

| Item | Role |
|------|------|
| `CircuitBreaker::new(threshold, reset_after)` | Fail fast after repeated errors; half-open probe after the cooldown (`call`, `state`) |
| `RateLimiter::new(rate, capacity)` | Token-bucket admission control (`acquire`) |
| `Bulkhead::new(n)` | Cap concurrent in-flight effects on a `Semaphore` (`with_permit`) |
| `hedged(primary, backup, delay)` | `primary.race(sleep(delay).then(backup))`: the first *success* wins |
| `retry_with_breaker(factory, schedule, &breaker)` | `Effect::retry` that stops once the breaker opens |

```rust
use std::time::Duration;
use id_effect::{Effect, Schedule, fail};
use id_effect_resilience::{CircuitBreaker, retry_with_breaker};

let breaker = CircuitBreaker::new(3, Duration::from_secs(30));
let call = || -> Effect<&'static str, &'static str> { fail("503") };
let out = retry_with_breaker(
    call, // a factory: effects are one-shot, so each attempt builds a fresh one
    Schedule::exponential(Duration::from_millis(100)).intersect(Schedule::recurs(5)),
    &breaker,
)
.block_on();
```

Failures of a guarded call are `Or<CircuitBreakerError, E>`: `Or::Left(CircuitBreakerError)` when the breaker is open. Defects and interrupts pass through without being counted or retried. Runnable examples: `cargo run -p id_effect_resilience --example 010_retry_with_breaker` (and `020_rate_limit`).

Pair resilience primitives with [`Schedule`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/scheduling/schedule.rs) retry policies from Part III — breakers shed load, schedules handle transient faults.
