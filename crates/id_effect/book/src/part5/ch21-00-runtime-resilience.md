# Runtime Resilience

Production services need more than happy-path effects. Timeouts, overload, and flaky dependencies are normal. This chapter covers coordination primitives and the `id_effect_resilience` crate for keeping programs responsive under stress.

## What This Chapter Covers

- **[`RequestResolver`](../../src/coordination/request_resolver.rs)** — batch parallel lookups through a single fetch
- **[`SubscriptionRef`](../../src/coordination/subscription_ref.rs)** — shared state plus change notifications
- **[`Redacted`](../../src/schema/redacted.rs)** — schema-layer secrets with masked `Debug`
- **[`match_effect!`](../../src/match_effect.rs)** — enum match helper with path-prefixed arms
- **`id_effect_resilience`** — circuit breaker, rate limiter, bulkhead, hedged requests

## RequestResolver

Effect.ts batches data-source lookups so N parallel `getUser(id)` calls become one SQL `WHERE id IN (…)`. In id_effect, each pending lookup is a [`RequestEntry`] with a [`Deferred`] result slot. A [`RequestResolver::run_all`] receives sequential batches; entries inside a batch may run in parallel.

[`batching`](../../src/coordination/request_resolver.rs) deduplicates keys per batch and calls your fetch function once:

```rust
use id_effect::{Deferred, RequestEntry, batching, run_async};

let resolver = batching(|keys| Effect::new(move |_r| {
    // build HashMap from keys …
    Ok(map)
}));
```

## SubscriptionRef

[`SubscriptionRef`] combines [`Ref`] with [`PubSub`]. Every `set` / `update` publishes the new value; [`subscribe`](../../src/coordination/subscription_ref.rs) returns a [`Queue`] that receives the current value first, then every subsequent change.

```rust
use id_effect::{Scope, SubscriptionRef, run_async};

let cell = run_async(SubscriptionRef::make(0u32), ()).await?;
let scope = Scope::make();
let changes = run_async(cell.subscribe(), scope.clone()).await?;
```

## Redacted values

Use [`Redacted<T>`] anywhere a schema or domain type might reach logs. `Debug` and `Display` print `<redacted>`; call [`expose`](../../src/schema/redacted.rs) only at trust boundaries.

## match_effect!

The [`match_effect!`](../../src/lib.rs) proc macro prefixes variant names so the compiler checks exhaustiveness without repeating the enum path:

```rust
use id_effect::match_effect;

match_effect!(Color, paint, {
    Red(n) => n,
    Green => 0,
    Blue => 1,
})
```

## id_effect_resilience

Add `id_effect_resilience` to your workspace dependency when you need operational guardrails:

| Type | Role |
|------|------|
| [`CircuitBreaker`] | Fail fast after repeated errors; half-open probe after cooldown |
| [`RateLimiter`] | Token-bucket admission control |
| [`Bulkhead`] | Cap concurrent in-flight effects via semaphore |
| [`hedged`] | Race a delayed backup effect against a primary |

```rust
use id_effect_resilience::{CircuitBreaker, hedged};
use id_effect::run_async;

let breaker = run_async(CircuitBreaker::make(5, Duration::from_secs(30)), ()).await?;
let out = run_async(breaker.call(fetch_user(id)), ()).await?;
```

Pair resilience primitives with [`Schedule`](../../src/scheduling/schedule.rs) retry policies from Part III — breakers shed load, schedules handle transient faults.
