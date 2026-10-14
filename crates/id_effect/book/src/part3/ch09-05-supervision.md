# Supervision — Restart Policies and Scope

Raw [`run_fork`](../appendix-a-api-reference.md) gives you a [`FiberHandle`](../appendix-a-api-reference.md) and full control. Long-lived servers usually need **policies**: when a child effect fails, should you **retry**, **back off**, **give up**, or **substitute a default**? Effect.ts encodes these ideas in **supervisors**. `id_effect` provides the same vocabulary wired to [`Scope`](./ch10-02-scopes-finalizers.md), [`CancellationToken`](./ch09-03-cancellation.md), and [`Schedule`](./ch11-01-schedule.md).

## Why not only `retry`?

[`retry`](./ch11-03-retry-repeat.md) re-runs an `Effect<A, E, R>` until success or the schedule stops. Supervision adds:

- A **stable shutdown channel** ([`CancellationToken`]) installed when a **child [`Scope`]** closes, so cooperative loops exit with [`Cause::Interrupt`](./ch08-02-exit.md) instead of spinning forever.
- **Declarative policies** (`Terminate`, `Restart`, `RestartWithLimit`, `Escalate`, `Ignore`) that compose with **virtual time** ([`TestClock`](../part4/ch15-02-test-clock.md)) for deterministic tests.

## `Supervisor` and `Scope`

`Supervisor::attach(parent_scope)` forks a child [`Scope`]. When the parent closes, the child closes; a **finalizer** on the child cancels the supervisor token so any `supervised` loop observes cancellation on the next iteration header.

Use `Supervisor::detached()` for examples and unit tests that do not need a parent tree.

## Policies in one table

| Policy | On child `Ok(a)` | On child `Err(e)` |
|--------|------------------|-------------------|
| `Terminate` / `Escalate` | Return `a` | Return [`Cause::Fail(e)]` (no retry) |
| `Restart { schedule }` | Return `a` | Sleep per `schedule`, run factory again |
| `RestartWithLimit { limit, schedule }` | Return `a` | Retry while under `limit`; then fail with [`Cause::Then`](./ch08-04-accumulation.md) aggregating prior failures |
| `Ignore { recover }` | Return `a` | Return `recover` |

`RestartWithLimit` counts **retries after failure**: `limit == 0` means “no retries” (fail on the first `Err` without sleeping). A positive limit allows that many **retry attempts** after the initial failing run.

## Typed failures vs interrupts vs defects

- A supervised child that returns [`Err`] becomes [`Cause::Fail`].
- Token cancellation (scope teardown or explicit `cancel`) surfaces as [`Cause::Interrupt`].
- Panics inside the interpreter are still **defects** at the runtime boundary; supervision does not “recover” unwinding `std` panics—document and test the happy paths your runtime actually exposes.

## `supervised` vs `FiberHandle::scoped`

[`FiberHandle::scoped`](./ch09-02-spawning-joining.md) ties **one handle** to **one scope** via a finalizer that **interrupts** the fiber. `supervised` runs the factory **inline** on your environment `R`, so it suits **retry loops** without an extra `FiberHandle` until you opt into `Supervisor::spawn`.

## Example sketch

```rust
use id_effect::{
  Supervisor, SupervisorPolicy, supervised,
  Schedule, TestClock, succeed,
};
use std::time::Instant;

let parent = id_effect::Scope::make();
let sup = Supervisor::attach(&parent);
let clock = TestClock::new(Instant::now());
let body = supervised(
  &sup,
  SupervisorPolicy::Restart {
    schedule: Schedule::spaced(std::time::Duration::ZERO),
  },
  clock,
  || succeed::<u32, &str, ()>(42),
);
// run `body` with your environment; close `parent` to cancel via token.
```

For production delays, prefer a non-zero [`Schedule`] and a real [`Clock`](./ch11-04-clock-injection.md) (for example the Tokio bridge clock in server code).

## Supervision layering

The workspace has exactly one supervision story, in three layers. Each layer builds on the one below it rather than re-implementing restart logic:

| Layer | Crate / type | Supervises | Responsibility |
|-------|--------------|------------|----------------|
| 1. Primitive | `id_effect::concurrency::Supervisor` + `SupervisorPolicy` | one effect body / fiber | Restart policy (`Terminate`, `Restart { schedule }`, bounded restart that escalates) and `RestartIntensity` accounting, tied to a `Scope` |
| 2. Process trees | `id_effect_process` `SupervisorSpec` / `SupervisorProcess` | child processes on one node | OTP-style trees: `OneForOne` / `OneForAll` / `RestForOne`, per-child restart and shutdown policies. Restart-intensity math is delegated to layer 1 |
| 3. Placement | `id_effect_node::DistributedSupervisor` | children across a mesh | Chooses *which node* a child runs on (via `ClusterResourcePolicy` / `place`) and re-places it when a node leaves |

Pick the lowest layer that covers your failure domain: a flaky effect → layer 1; a set of cooperating actors on one machine → layer 2; work that must survive losing a machine → layer 3 (see [Processes and supervision](../part7/ch37-00-processes-supervision.md) and [Cluster groups and takeover](../part7/ch39-00-cluster-supervision.md)).

**`FabricGovernor` is not a supervisor.** The Compute Fabric's governor (formerly `ComputeSupervisor`, see [Compute Fabric](./ch12-00-compute-fabric.md)) does *resource governance* — it samples telemetry, throttles admission, sizes thread pools and emits scale-out jobs. It never restarts failed work. It was renamed in 0.6 precisely so it is not mistaken for a fourth supervision layer.

## See also

- [Phase F parity doc](https://github.com/Industrial/id_effect/blob/main/docs/effect-ts-parity/phases/phase-f-supervision.md) — epic breakdown and acceptance notes.
- [`supervisor.rs`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/concurrency/supervisor.rs) — full API and unit tests.
