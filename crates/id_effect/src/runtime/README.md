# `runtime` — Stratum 6: executing effects

The **interpreter** that runs [`Effect`](../kernel/README.md): synchronous [`run_blocking`](execute.rs), asynchronous [`run_async`](execute.rs), [`Never`](execute.rs) for uninhabited errors, and [`Runtime`](rt.rs) / [`run_fork`](rt.rs) / [`yield_now`](rt.rs) for scheduling integration.

Concurrency primitives ([`FiberId`](../concurrency/README.md), etc.) live in [`concurrency`](../concurrency/mod.rs) but are **re-exported** from `runtime` for stable `crate::runtime::FiberId` paths.

## What lives here

| Module | Role |
|--------|------|
| `execute` | `run_blocking`, `run_async`, `Never` — drive `Effect` to completion. |
| `rt` | `Runtime`, `ThreadSleepRuntime`, `run_fork`, `yield_now` — fork fibers / cooperate with scheduler. Fork uses a [`Send`] factory `spawn_with` / `run_fork(\|\| (effect, env))` so [`Effect`](../kernel/effect.rs) need not be [`Send`]. |

## What it is used for

- **`main`, tests, and top-level binaries** — the only places that should call `run_*` on full application graphs.
- **Bridging** to async executors via `run_async` when you need `Future` output.
- **Spawning** child work with `run_fork` when fiber handles are required.

## Best practices

1. **One boundary** — push `run_blocking` / `run_async` to the edge; library code returns `Effect`.
2. **Pick `run_blocking` vs `run_async`** based on caller; do not nest ad hoc executors inside small helpers.
3. **Environment `R`** must be fully built before `run_*` unless your effect internally extends it (advanced).
4. **Tests** — use [`testing::run_test`](../testing/README.md) helpers for deterministic harnesses when available.

## See also

- [`SPEC.md`](../../SPEC.md) §Stratum 6.
- [`kernel`](../kernel/README.md) — what is being executed.
- [`concurrency`](../concurrency/README.md) — fiber types re-exported here.
