---
name: id_effect-resilience
description: >-
  Runtime resilience for id_effect: RequestResolver batching, SubscriptionRef,
  Redacted schema values, match_effect!, and id_effect_resilience (circuit breaker,
  rate limiter, bulkhead, hedged). Use when building data loaders, reactive state,
  secret handling, or overload protection.
---

# id_effect Resilience

**Part V ch21**. Coordination + schema helpers in `id_effect`; operational patterns in `id_effect_resilience`.

**Prerequisite**: `id_effect-fundamentals`, `id_effect-concurrency`.

## Decision tree

```
Batch many parallel lookups into one fetch?
  → RequestResolver + batching(fetch)

Shared state + subscribers need every change?
  → SubscriptionRef (Ref + PubSub)

Secret in domain/schema type?
  → Redacted<T> (masked Debug/Display)

Enum dispatch with less boilerplate?
  → match_effect!(Enum, expr, { Variant => … })

Overload / flaky dependency protection?
  → id_effect_resilience crate
```

## RequestResolver

```rust
use id_effect::{Deferred, RequestEntry, batching, run_async};

let resolver = batching(|keys| Effect::new(move |_r| Ok(fetch_map(keys))));
// run_all(vec![vec![entry1, entry2, …]]) completes each entry.deferred
```

## SubscriptionRef

```rust
use id_effect::{Scope, SubscriptionRef, run_async};

let cell = run_async(SubscriptionRef::make(initial), ()).await?;
let q = run_async(cell.subscribe(), Scope::make()).await?;
run_async(cell.set(new_value), ()).await?; // subscribers receive update
```

## Redacted

```rust
use id_effect::Redacted;

let key = Redacted::new(token);
// format!("{key:?}") => "<redacted>"
let wire = key.expose().clone(); // auditable boundary only
```

## match_effect!

```rust
use id_effect::match_effect;

let n = match_effect!(Msg, msg, {
    Ping(x) => x,
    Pong => 0,
});
```

## id_effect_resilience

```toml
id_effect_resilience = { path = "../id_effect_resilience", version = "0.3.0" }
```

```rust
use id_effect_resilience::{CircuitBreaker, RateLimiter, Bulkhead, hedged};
use id_effect::failure::Or;

let cb = run_async(CircuitBreaker::make(3, Duration::from_secs(10)), ()).await?;
match run_async(cb.call(risky_call()), ()).await {
  Ok(v) => …,
  Err(Or::Left(CircuitBreakerError)) => …,
  Err(Or::Right(e)) => …,
}
```

## Verify

```bash
cargo test -p id_effect -p id_effect_resilience
cargo clippy -p id_effect_resilience -- -D warnings
```

## See also

- Book: `crates/id_effect/book/src/part5/ch21-00-runtime-resilience.md`
- Coordination: `id_effect-concurrency` skill (PubSub, Ref, Deferred)
- Scheduling retry: `id_effect-concurrency` / Part III ch11
