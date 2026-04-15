# `resource` — Stratum 8: scopes & resource lifetimes

**Safe acquisition and release** of resources in an effectful program: [`Scope`](scope.rs) with [`Finalizer`](scope.rs), [`Pool`](pool.rs) / [`KeyedPool`](pool.rs), and [`Cache`](cache.rs) with [`CacheStats`](cache.rs).

## What lives here

| Module | Role |
|--------|------|
| `scope` | `Scope`, `Finalizer` — bracketed acquire/use/release; integrates with STM/latch as needed. |
| `pool` | `Pool`, `KeyedPool` — reuse expensive resources (connections, clients). |
| `cache` | `Cache`, `CacheStats` — memoization / eviction policies over keys. |

## What it is used for

- **Guaranteeing** finalizers run when leaving a dynamic region (even on failure).
- **Bounding** parallelism or cost via pools instead of unbounded `spawn`.
- **Sharing** read-heavy data with explicit invalidation via cache.

## Best practices

1. **Prefer `Scope`** over manual `try/finally` in `Effect` graphs — finalizers compose with the interpreter.
2. **Size pools** from real concurrency limits (DB, FDs); expose metrics via [`observability`](../observability/README.md).
3. **Do not** store `Effect` inside caches unless you fully control invalidation and thread safety.
4. **Test** scope exit paths — use [`testing::assert_no_unclosed_scopes`](../testing/README.md) where applicable.

## See also

- [`SPEC.md`](../../SPEC.md) §Stratum 8.
- [`coordination`](../coordination/README.md) — queues/semaphores often sit beside pools.
- [`concurrency`](../concurrency/README.md) — fibers and cancellation interact with scoped work.
