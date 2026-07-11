# ADR 0007 — Compute Fabric

## Status

Accepted

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
| `ComputeSupervisor` | Control loop: monitor → admit → place → rebalance |
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

- Default `run_blocking` / `run_async` install Fabric layer (escape hatch: `run_blocking_serial` for tests)
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
