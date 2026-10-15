# Supervision — Restart Policies and Supervisors

`fork` gives you a `FiberHandle` and full control. Long-lived services usually need **policies**: when a child effect fails, should you **restart** it, **give up** after a few tries, or **substitute a default**? Effect.ts and OTP encode these ideas in **supervisors**; id_effect provides the same vocabulary on fibers, in `id_effect::coordination::supervisor` (re-exported at the root).

## Why not only `retry`?

[`Effect::retry`](./ch11-03-retry-repeat.md) re-runs an effect on *typed* failure as a `Schedule` allows. Supervision adds:

- **Defects are restarted too** (a panicking worker comes back); only an **interrupt** is never restarted, so interrupting the loop stops it.
- **Ownership**: a `Supervisor` owns its fibers through a [`Scope`](./ch10-02-scopes-finalizers.md). `shutdown()` — or closing a parent scope it is attached to — interrupts and awaits every supervised fiber.
- **Declarative policies** (`Terminate`, `Restart`, `RestartWithLimit`, `Escalate`, `Ignore`) and an OTP-style `RestartIntensity` limiter.

## `supervised(policy, make)`

`supervised` runs the effects built by `make` (an `FnMut() -> Effect<A, E, R>`, called once per attempt) under a `SupervisorPolicy<A>`:

| Policy | On success `a` | On failure (typed or defect) |
|--------|----------------|------------------------------|
| `Terminate` / `Escalate` | return `a` | propagate the first failure |
| `Restart { delay }` | return `a` | pause `delay`, run `make()` again |
| `RestartWithLimit { limit, delay }` | return `a` | retry up to `limit` times, then fail with every failure so far sequenced with `Cause::Then` |
| `Ignore { recover }` | return `a` | succeed with `recover` |

`RestartWithLimit { limit: 0, .. }` fails on the first failure. A `delay` of `Duration::ZERO` yields the fiber between attempts instead of sleeping. Delays are slept on the executor clock, so they are virtual time on `TestExecutor`. (The policies take a fixed `delay`; for backoff schedules on typed failures, wrap the body in `Effect::retry`.)

```rust
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use id_effect::{Effect, Exit, SupervisorPolicy, fail, succeed, supervised};

let attempts = Arc::new(AtomicUsize::new(0));
let a = Arc::clone(&attempts);
let flaky = move || {
    if a.fetch_add(1, Ordering::SeqCst) < 2 { fail("crashed") } else { succeed(42_u8) }
};
let eff: Effect<u8, &str> =
    supervised(SupervisorPolicy::Restart { delay: Duration::ZERO }, flaky);
assert_eq!(eff.block_on(), Exit::succeed(42));
assert_eq!(attempts.load(Ordering::SeqCst), 3);
```

## `Supervisor`

A `Supervisor` owns supervised fibers:

```rust,ignore
use id_effect::{Scope, Supervisor, SupervisorPolicy};

let parent = Scope::new();
let sup = Supervisor::new();
let setup = sup
    .attach_to::<Never, _>(&parent) // closing `parent` shuts `sup` down
    .then(sup.spawn(SupervisorPolicy::Restart { delay: Duration::from_secs(1) }, make_worker));
// ... later: sup.shutdown() interrupts and awaits every supervised fiber.
```

- `spawn(policy, make)` forks `supervised(policy, make)` into the supervisor's scope and returns its `FiberHandle`; the fiber outlives the forking fiber.
- `fork(eff)` forks any effect under the supervisor without a restart loop.
- `shutdown()` is idempotent; forking into a shut-down supervisor yields a fiber that is interrupted before it runs.
- `scope()` is the supervisor's `Scope`, for resources that should live exactly as long as it.

`RestartIntensity::new(max_restarts, within)` is the pure sliding-window limiter (`note(now)` returns `false` once the restarts inside the window exceed the budget); process trees use it to decide when to escalate.

## Supervision layering

The workspace has one supervision story, in three layers. Each builds on the one below it rather than re-implementing restart logic:

| Layer | Crate / type | Supervises | Responsibility |
|-------|--------------|------------|----------------|
| 1. Primitive | `id_effect::{supervised, Supervisor, SupervisorPolicy, RestartIntensity}` | one effect body / fiber | restart policy and restart-intensity accounting, tied to a `Scope` |
| 2. Process trees | `id_effect_process` `SupervisorSpec` / `SupervisorProcess` | child processes on one node | OTP-style trees: `OneForOne` / `OneForAll` / `RestForOne`, per-child restart and shutdown policies; restart intensity is layer 1's `RestartIntensity` |
| 3. Placement | `id_effect_node::DistributedSupervisor` | children across a mesh | chooses *which node* a child runs on (via `ClusterResourcePolicy` / `place`) and re-places it when a node leaves |

Pick the lowest layer that covers your failure domain: a flaky effect → layer 1; a set of cooperating actors on one machine → layer 2; work that must survive losing a machine → layer 3 (see [Processes and supervision](../part7/ch37-00-processes-supervision.md) and [Cluster groups and takeover](../part7/ch39-00-cluster-supervision.md)).

**`FabricGovernor` is not a supervisor.** The Compute Fabric's governor ([Compute Fabric](./ch12-00-compute-fabric.md)) does *resource governance* — it samples telemetry, throttles admission, sizes thread pools and emits scale-out jobs. It never restarts failed work.

## See also

- [Phase F parity doc](https://github.com/Industrial/id_effect/blob/main/docs/effect-ts-parity/phases/phase-f-supervision.md) — epic breakdown and acceptance notes.
- [`supervisor.rs`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/coordination/supervisor.rs) and [`tests/coordination_supervisor.rs`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/tests/coordination_supervisor.rs) — full API and tests.
