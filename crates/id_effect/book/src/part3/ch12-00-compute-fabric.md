# Compute Fabric

The Compute Fabric is id_effect's resource-aware execution substrate. It has two layers:

- **`Fabric`** (always built) — the kernel's **fiber admission scheduler**. Children forked with `Concurrency::Inherit` (by `fork_all`, and so by `all`, `for_each_par`, `zip_par`, …) first await a permit from the *ambient* fabric. This is how the fabric limits how much parallel work runs at once.
- **The resource-aware substrate** (cargo feature `compute`, on by default) — a `ComputeFabric` with a `FabricGovernor` that compares live telemetry against a declarative `ResourcePolicy` and adjusts admission, Rayon pool sizing and placement.

## Fiber Admission with `Fabric`

```rust
use std::sync::Arc;
use id_effect::{Concurrency, Effect, Fabric, Never, TestExecutor, all, succeed};

let fabric = Fabric::with_capacity(2); // at most 2 `Inherit` children run at once
let work: Vec<Effect<u32, Never>> = (0..8).map(succeed).collect();
let eff = fabric.provide(all(work, Concurrency::Inherit));

let exec = TestExecutor::new();
let h = eff.fork_root(Arc::new(exec.clone()));
exec.run_until_stalled();
assert!(h.is_done());
```

- The ambient fabric is **fiber-local** (like Effect.ts `Effect.withConcurrency`): `fabric.provide(eff)` sets it for `eff` and the children it forks; `Fabric::current()` reads it. It is not a capability in `R`, so any effect can use `Inherit` whatever its environment.
- With no ambient fabric, `Concurrency::Inherit` behaves as `Unbounded`. `Concurrency::Bounded(n)` is a purely local window and never consults the fabric.
- Each admitted child holds one permit while it runs and gives it back however it exits. Waiting for a permit is a suspended future (no thread is held) and is interruptible; nested `Inherit` forks do not deadlock (a parent lends its permit to one child at a time).
- Constructors: `Fabric::with_capacity(n)`, `Fabric::from_compute(Arc<ComputeFabric>)`, `Fabric::from_controller(Arc<AdmissionController>)`, or `Fabric::new(custom)` for your own `Admission` implementation.

## Declarative Policy

```rust
use id_effect::compute::{MetricMode, MetricPolicy, RebalanceStrategy, ResourcePolicy};

let policy = ResourcePolicy {
    memory: MetricPolicy::new(MetricMode::Max { ceiling: 0.85 }),
    cpu: MetricPolicy::new(MetricMode::Max { ceiling: 1.0 }),
    rebalance: RebalanceStrategy::ThrottleAdmission,
};

let same = ResourcePolicy::memory_cap_max_cpu(0.85);            // the policy above
let spread = ResourcePolicy::unlimited_memory_cpu_spread(0.25); // even CPU share per worker
```

`MetricMode` is `Max { ceiling }`, `Target { setpoint }`, `Spread { per_worker }` or `Unlimited`; `RebalanceStrategy` is `ThrottleAdmission` (default), `ShedLoad`, `ScaleOut` or `ScaleIn`.

## The Governor Loop

`FabricGovernor` samples a `TelemetryEngine` (live `sysinfo` on a host; a mock in tests via `ComputeFabric::with_mock(policy, cpu, mem)`) and compares the readings to the policy on each `tick()`:

1. **Monitor** — a `TelemetrySnapshot { cpu_pct, mem_pct }`;
2. **Admit** — the `AdmissionController` adjusts its permit count;
3. **Size** — the Rayon pool and the auto-parallel threshold follow the budget;
4. **Rebalance** — throttle, shed, scale out or scale in.

Each tick records a `ComputeEvent::SupervisorTick` in the [tracing buffer](../part6/ch27-00-observability.md). Example: memory at 61 % with an 85 % ceiling → headroom → many permits; memory at 90 % → admission throttled. The governor does resource governance only — restarting failed work is the [`Supervisor`](./ch09-05-supervision.md)'s job.

Put a `ComputeFabric`'s admission in front of fibers with `Fabric::from_compute` (from `examples/120_compute_fabric_memory_cap.rs`):

```rust
use std::sync::Arc;
use id_effect::compute::ComputeFabric;
use id_effect::{Concurrency, Effect, Fabric, all, succeed};

let fabric = Arc::new(ComputeFabric::memory_cap_max_cpu(0.85));
fabric.governor().tick();
let work: Vec<Effect<u32, ()>> = (0..4).map(|_| succeed(42)).collect();
let out = Fabric::from_compute(fabric)
    .provide(all(work, Concurrency::Inherit))
    .block_on();
# assert_eq!(out, id_effect::Exit::succeed(vec![42; 4]));
```

## Adaptive Parallelism

`AdaptiveContext` holds the current admission budget, Rayon thread count and auto-parallel element threshold. Bulk dispatch through `parallel_if_profitable(len, serial, parallel)` / `should_parallelize(len)` consults it, so pure chunk operations (`Stream::map`, `filter`, …) go parallel only when the chunk is large enough (ADR 0008).

- `configure_rayon_threads` — governor-sized Rayon pool;
- `CpuSpreadBucket` — token bucket for `MetricMode::Spread` (`examples/121_compute_fabric_cpu_spread.rs`);
- `Stream::map_effect_par(n, f)` runs element effects on child fibers; under a fabric-backed `Fabric` the number running at once is also capped by its permits (`examples/122_compute_fabric_effect_parallel.rs`).

See also [Implicit Parallelism](../part4/ch13-05-parallelism.md).

## Cluster

When the local fabric saturates, `RebalanceStrategy::ScaleOut` builds a `FabricJobSpec` via `FabricGovernor::scale_out` (or `id_effect_node::placement::scale_out_placed`, which records a `target_node`). The labs crate `id_effect_jobs` (unpublished, see [ADR 0010](https://github.com/Industrial/id_effect/blob/main/docs/adrs/0010-crate-tiers.md)) converts one with `JobSpec::from_fabric` and runs it on a `FabricJobRunner`; its example `labs/id_effect_jobs/examples/030_compute_fabric_cluster.rs` shows a two-node offload. Within the published crates, cross-node placement goes through `id_effect_node` (Part VII).

Cluster placement lives in `id_effect_node::placement`. `ClusterResourcePolicy` combines global and per-node caps with a `PlacementMode`; every placement decision flows through one pure entry point, `place(reports, policy)`, fed by `LoadReport` telemetry:

| `PlacementMode` | Chooses |
|-----------------|---------|
| `Affinity { node }` | an explicitly preferred node when eligible |
| `LocalFirst` | the eligible node with the most policy headroom |
| `Spread` | the eligible node with the lowest raw load |
| `RoundRobin` | the next node in a deterministic rotation |

## Further reading

- [ADR 0007](https://github.com/Industrial/id_effect/blob/main/docs/adrs/0007-compute-fabric.md) (Amendment 1: the fabric as the fiber admission scheduler)
- [ADR 0008](https://github.com/Industrial/id_effect/blob/main/docs/adrs/0008-implicit-parallelism.md) — Fabric-only parallelism
- [Concurrency](./ch09-00-concurrency.md), [Scheduling](./ch11-00-scheduling.md) — temporal vs compute scheduling
