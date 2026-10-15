# ADR 0007 — Compute Fabric

## Status

Accepted, amended 2026-10-15 for 0.7 (see
[Amendment 1](#amendment-1-2026-10-15-phase-c-for-07)): Phase C is redefined around the
0.7 fiber runtime ([ADR 0011](0011-fiber-runtime-send-typed-r.md)).

## Context

id_effect has structured concurrency (fibers, scopes, supervision) but no unified compute governance:

- `ThreadSleepRuntime` spawns one OS thread per fiber
- Rayon parallelism uses a fixed size threshold (`Parallelism::Auto { threshold: 1024 }`, ADR 0006)
- Resource limits are local (pools, semaphores, `map_par_n`) with no global feedback loop
- Failure `Supervisor` handles restart policies, not resource placement
- Stratum 10 scheduling is temporal (retry/backoff), not compute scheduling

## Decision

### Core principle

**Every effect execution passes through Compute Fabric** at the runtime boundary. Programs still build lazy descriptions; Fabric decides where, when, and how concurrently work runs — locally or on cluster workers — subject to `ResourcePolicy`.

### New stratum: `compute/` (5.5)

| Component | Role |
|-----------|------|
| `ResourcePolicy` | Declarative CPU/memory rules (`MetricMode`: Max, Target, Spread, Unlimited) |
| `TelemetryEngine` | Live CPU/memory feedback (sysinfo; cluster aggregation later) |
| `FabricGovernor` (was `ComputeSupervisor`) | Control loop: monitor → admit → place → rebalance |
| `AdmissionController` | Global/per-metric semaphores driven by telemetry vs policy |
| `WorkProfile` | Placement hint: CpuIntensive, IoBound, MemoryHeavy, Remote, Mixed |
| `ComputeFabric` | Capability installed at `run_with` / `run_async` boundary |

### Local executors (unified)

- **FiberPool** — work-stealing pool replacing one-thread-per-fiber
- **RayonComputePool** — thread count + threshold from supervisor snapshot
- **IoRuntime** — Tokio blocking pool size tied to CPU policy

### Effect Dependency Graph (EDG) — Phase C

Extend `effect!` proc-macro to analyze bind dependencies. Independent `~` steps may codegen parallel groups via `Effect::par_all` / Fabric admission. Safety gates:

- Data dependency analysis (SSA-style on locals)
- No overlapping `&mut R` capability borrows in parallel groups
- `#[effect(serial)]` opt-out
- Conservative unknown → sequential fallback

### Relationship to ADR 0006

ADR 0006 makes bulk collection ops parallel-by-default via Rayon threshold. ADR 0007 **supersedes the implicit caller-picks-concurrency model**: threshold and thread count become supervisor-driven; `effect!` may auto-parallelize independent binds when EDG analysis permits. Sequential fallback when Fabric disabled or analysis conservative.

### Cluster (Phase E)

When local Fabric saturates, `RebalanceStrategy::ScaleOut` enqueues `JobSpec` via `id_effect_jobs`; durable steps via `id_effect_workflow` journal. Workers run node-local Fabric under `ClusterResourcePolicy`.

## Consequences

- Default `run_blocking` / `run_async` install Fabric layer (escape hatch: `run_blocking_serial` for tests — never built; see [ADR 0008 Amendment 1](0008-implicit-parallelism.md#amendment-1-2026-10-14))
- Orthogonal to failure `Supervisor` — Fabric wraps spawn/placement; supervision unchanged
- Non-Send parallel groups clone `R` on worker threads (`R: Clone + Send + Sync`)
- Supervisor poll interval configurable; fast-path when policy is `Unlimited`
- Book Part III chapter on Compute Fabric; migration notes from ADR 0006 manual parallelism

## Phased delivery

| Phase | Scope |
|-------|-------|
| A | ADR, spec, policy types, telemetry, supervisor skeleton |
| B | FiberPool, admission, runtime refactor, Tokio wrapper |
| C | EDG proc-macro, `Effect::par_all`, compile-time safety gates |
| D | Adaptive `Parallelism`, `map_par_adaptive`, CPU spread mode |
| E | Cluster placement via jobs/workflow |
| F | Default-on Fabric, docs, skills, examples |

## Amendment 1 (2026-10-15, Phase C for 0.7)

Redefines Phase C for the 0.7 fiber runtime (mission `id-effect-runtime-0-7`, task
`w2-adr-0007-amend`; brief D6, `.maestro/specs/0.7/kernel-design-a-hlist.md` §4). Where
the Phase C row, the EDG section or the Consequences above disagree with this section,
this section wins.

### Context in 0.6

- `Effect::par_all` was never built. The EDG part of Phase C shipped as the `effect!`
  auto-parallel `join_binds2/3/4` (ADR 0008), which use `join`, not a fail-fast try-join
  (`crates/id_effect/src/kernel/par_bind.rs:13,22,37`).
- Admission is thread-blocking. `AdmissionController::acquire_blocking` spins on
  `try_acquire` with `thread::yield_now` (`crates/id_effect/src/compute/admission.rs:47-51`).
  `ComputeFabric::spawn_admitted` calls it before handing work to the `FiberPool`
  (`compute/fabric.rs:95-104`, used by `runtime/rt.rs:115`), and `TokioRuntime` calls it
  inside a `spawn_blocking` closure, so a throttled fiber holds a blocking-pool thread
  while it waits (`crates/id_effect_tokio/src/lib.rs:192-196`).

### Decision: the Fabric is the scheduler behind `fork_all`

Every parallel combinator of the 0.7 kernel goes through one function:

```rust
pub(crate) fn fork_all<A, E, R>(ctx: &Ctx<R>, children: Vec<Effect<A, E, R>>, c: Concurrency)
  -> Effect<Vec<A>, E, R>;
pub enum Concurrency { Sequential, Bounded(usize), Unbounded, Inherit /* the Fabric decides */ }
pub trait Admission: Send + Sync { fn admit(&self, w: Weight) -> BoxFuture<'static, Permit>; }
```

- **Callers.** `zip_par`, `all(…, concurrency)`, `for_each_par`, the `effect!`
  auto-parallel `join_binds*` and `Stream::map_effect_par` (ADR 0008 Amendment 2) all call
  `fork_all`. It forks the children under the current fiber, so the fiber tree and the
  interruption rules of ADR 0011 apply: on the first failure it interrupts and awaits the
  siblings, then fails.
- **Admission is asynchronous.** With `Concurrency::Inherit` or `Bounded`, each fork first
  awaits `fabric.admission().admit(w)`. The returned `Permit` is released when the child
  exits. A throttled child is a suspended future with a registered waker; it holds no
  thread. `Inherit` is the hook that hands the concurrency decision to the Fabric's
  `ResourcePolicy`.
- **`AdmissionController` gains an async `admit`** next to `acquire_blocking`. The
  blocking variant is kept for `spawn_blocking` work only; the 0.7 kernel never calls it
  (`rg acquire_blocking crates/id_effect/src/next` = 0, task `c-fabric-scheduler`).
- **Executors.** `ComputeFabric::spawn_admitted` is re-expressed as a decorator,
  `FabricExecutor<X: Executor>`, over the ADR 0011 `Executor`. `FiberPool` keeps CPU-bound
  compute work only; ordinary fibers run on the executor (tokio by default).

### What this replaces

| Phase C (original) | 0.7 |
|---|---|
| `Effect::par_all` for EDG parallel groups | **Superseded** by `fork_all` and its public combinators (`zip_par`, `all`, `for_each_par`) |
| Fabric admission via the thread-blocking `acquire_blocking` | async `Admission::admit` returning a `Permit` |
| "No overlapping `&mut R` capability borrows in parallel groups" | moot: 0.7 effects read `&R`, and children share the env through `Ctx<R>` (`Arc` clones) |
| Consequence "non-`Send` parallel groups clone `R` on worker threads (`R: Clone + Send + Sync`)" | dropped: effects are `Send` by default, and `R` is shared, not cloned |

The EDG analysis, `#[effect(serial)]` and the conservative sequential fallback stay as
ADR 0008 describes. Phases D to F are unchanged.

### Implementation

`fork_all` and `Concurrency` are part of the kernel (`k-rebuild`); the spike declared
`Admission` but did not wire it (`spikes/kernel_a/src/par.rs`). The public combinators
land in `c-zip-par-all` and `c-join-binds`. `c-fabric-scheduler` adds the async `admit`
and wires `Concurrency::Inherit` to it (`crates/id_effect/src/next/compute.rs`); its
acceptance test caps `all(Inherit)` at 2 on a capacity-2 fabric. The 0.6
`spawn_admitted` path stays in the v1 kernel until `k-retire-v1`.
