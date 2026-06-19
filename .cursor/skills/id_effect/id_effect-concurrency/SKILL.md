---
name: id_effect-concurrency
description: >-
  Teaches id_effect concurrency: fibers, fork/join, fiber_all/fiber_race/fiber_any,
  cancellation, FiberRef, supervision, scopes/finalizers, acquire_release, and Schedule
  retry/repeat. Use when spawning work, managing lifetimes, or injecting TestClock.
---

# id_effect Concurrency

**Part III** — fibers (ch9), resources/scopes (ch10), scheduling (ch11).

**Prerequisites**: `id_effect-fundamentals`, `id_effect-errors`.

## Fibers

```rust
let handle = compute().fork();
let local = other_work();
let remote = handle.join().await.into_result_or_panic()?;
```

| Combinator | Behavior on failure |
|------------|---------------------|
| `fiber_all` | Cancel remaining; return first error |
| `fiber_race` | Cancel remaining; first **success** wins |
| `fiber_any` | Wait for all; first success or all errors |
| `fork` + `join` | Individual fiber's `Exit` |

```rust
use id_effect::fiber_all;
let users: Vec<User> = run_blocking(fiber_all(ids.iter().map(|&id| fetch_user(id))))?;
```

**`effect!` loops are sequential.** Use `fiber_all` for concurrent independent effects.

## Cancellation

Interrupted fibers produce `Exit::Failure(Cause::Interrupt)`. Parent fibers should join children to avoid leaks — `run_test` detects unjoined fibers.

## FiberRef

Thread-local-like state that propagates across fibers. Use for request IDs, logging context, tracing baggage — not for shared mutable business state.

## Resources & scopes

```rust
// acquire_release — bracket resources with finalizers
// Scopes — nest finalizers; run on success, failure, or interrupt
// Pools — reuse expensive resources (connections, clients)
```

Prefer **`acquire_release`** over manual `Drop` when cleanup must run on effect failure or cancellation.

## Schedule

```rust
Schedule::exponential(100.ms()).take(3)
effect.retry(schedule)
effect.repeat(schedule)
```

Inject **`Clock`** capability for deterministic time in tests (`id_effect-testing`).

## Not this → but that

| Not this | But that |
|----------|----------|
| `tokio::spawn` fire-and-forget in domain | `fork` + structured `join` |
| `Task::spawn` without join in tests | `run_test` leak detection |
| Shared `Arc<Mutex<T>>` for request context | `FiberRef` |
| Manual `sleep` in retry tests | `TestClock` + `run_test` |

## Next

- Streams (different concurrency model): [id_effect-streams](../id_effect-streams/SKILL.md)
- Testing: [id_effect-testing](../id_effect-testing/SKILL.md)
