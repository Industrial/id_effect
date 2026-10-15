# ADR 0008 — Implicit parallelism (API collapse)

## Status

Accepted, amended 2026-10-14 (see [Amendment 1](#amendment-1-2026-10-14)) and
2026-10-15 (see [Amendment 2](#amendment-2-2026-10-15))

## Context

ADR 0006 introduced `Parallelism` and `*_with(policy)` for caller-controlled Rayon dispatch. ADR 0007 added Compute Fabric with supervisor-driven thresholds (`should_parallelize_current`) and partial EDG auto-parallelism. Callers still choose among `map`, `map_with`, `map_par`, `map_serial`, `map_par_n`, and `map_par_adaptive`.

## Decision

### Core principle

**Fabric is the only parallelism control plane.** Bulk and effectful APIs use a single primary method; serial execution is an explicit opt-out.

### Public API (0.4.0)

| Operation | API | Escape hatch |
|-----------|-----|--------------|
| Pure bulk (`Vec`, `HashMap`, `Stream` chunks, …) | `map`, `filter`, `sort_with`, … | `*_serial` |
| Effectful stream | `Stream::map(f)` when `f: Fn(A) -> Effect<B, E, R>` | `map_serial` |
| `effect!` binds | auto-parallel independent sets via EDG | `#[effect(serial)]` |
| Determinism / tests | — | `run_blocking_serial` |

**Removed from public API:** `Parallelism`, `*_with`, `*_par`, `map_par_n`, `map_par_adaptive`, `compute::effective_threshold`.

### Dispatch

- All Rayon paths use `compute::dispatch::parallel_if_profitable` → `should_parallelize_current` + `install_parallel`.
- `run()` and `run_with()` install default Fabric.
- `ensure_run_context()` at all `run_blocking` / `run_async` boundaries.

### EDG

Independent bind **sets** (not only pairs) codegen via `join_binds2` / `join_binds3` / `join_binds4`. Capability-key `~` binds and overlapping dependencies stay sequential.

## Relationship to prior ADRs

- **Supersedes** ADR 0006 public `Parallelism` surface (bulk dispatch behavior retained).
- **Extends** ADR 0007 Phase F: unified `Stream::map`, Fabric on `run()`, EDG independent sets.

## Consequences

- **Breaking:** 0.4.0 removes deprecated and policy APIs.
- `*_serial` and `#[effect(serial)]` remain for `FnMut`, non-`Send`, and ordering.
- Remote/cluster placement unchanged (deferred).

## Amendment 1 (2026-10-14)

Reconciles this ADR with the code as of the 0.6 consolidation (mission
`id-effect-consolidation-0-6`, task `w1-adr0008-reconcile`). Where the table above
disagrees with this section, this section wins.

| Original decision | As built |
|---|---|
| Effectful stream via `Stream::map(f)` when `f: Fn(A) -> Effect<B, E, R>` (one unified `map`) | **Not built.** `Stream::map` takes pure `Fn(A) -> B` (Fabric-parallel per chunk). The effectful operator is `Stream::map_effect` (serial, ordered, `Or<E, E2>` errors). A unifying `StreamMapper` trait is dropped: two clearly named methods are simpler than overloading one name on closure return type. |
| `run_blocking_serial` for determinism and tests | **Not built, dropped.** Rayon paths preserve output order, so results are already deterministic. When side-effect *order* inside closures matters, use the `*_serial` operators or `#[effect(serial)]`. |
| `run()` and `run_with()` install the default Fabric | **Changed:** they call `ensure_default_fabric()`, which installs the default **only if no fabric is installed**. A caller-installed fabric is never replaced (downstream-reported defect, fixed in `w1-run-with`). `install_default_fabric()` still overwrites, and its docs say so. |
| Telemetry sampled when the Fabric is built | **Changed:** `SysinfoTelemetry` is lazy; nothing samples CPU until a placement or scale decision needs it (`w1-lazy-telemetry`). |
| Stream operators drain upstream per call | **Changed:** `Stream` is pull-based (`StreamState::Pull`, `Stream::from_pull`). Each operator pulls one upstream chunk per downstream pull, so the Fabric parallel branch applies per chunk and infinite sources work (`w1-pull-streams`). |

`parallel_if_profitable`, `ensure_run_context`, the EDG `join_binds2/3/4` codegen and
`#[effect(serial)]` exist as described.

## Amendment 2 (2026-10-15)

Corrects Amendment 1 and records the 0.7 decision (mission `id-effect-runtime-0-7`,
task `w2-adr-0008-amend`; release plan §9, default Q6). Where Amendment 1 disagrees
with this section, this section wins.

### Drift in Amendment 1

Amendment 1 calls `Stream::map_effect` "serial, ordered". The 0.6 code is not serial.
`map_effect` (`crates/id_effect/src/streaming/stream.rs`, around line 1193) takes
`admission_budget()` permits, then runs every element of a chunk through
`buffer_unordered(permits)` behind a `Semaphore`, and sorts the results by index.
So it is **ordered but concurrent**, bounded by the Fabric admission budget.
Effects inside one chunk run at the same time and their side effects interleave.

### Decision for 0.7

| Operator | Behaviour |
|---|---|
| `Stream::map_effect(f)` | **Sequential.** One element's effect completes before the next starts. Output order and side-effect order both match stream order. |
| `Stream::map_effect_par(n, f)` | **Bounded concurrency.** At most `max(n, 1)` effects in flight. Output order matches stream order; side-effect order does not. |

Why:

- The name `map_effect` should mean what `Effect`'s own sequencing means. Hidden
  concurrency in the default operator surprises callers whose effects touch shared
  state, and Amendment 1 shows that even the docs read it as serial.
- The caller picks the bound. The admission budget is a machine-wide number; it is
  the wrong default for I/O fan-out (for example, a database pool of 10).
- This matches Effect.ts, where `Stream.mapEffect` is sequential unless a
  `concurrency` option is given.

This narrows the ADR's core principle for effectful streams: concurrency is an
explicit opt-in (`map_effect_par`), not an opt-out. Pure bulk operators
(`Stream::map`, `Vec`/`HashMap` bulk ops) keep Fabric-driven parallelism.

### Implementation

The code change lands in `m-streams`, when streaming moves to the 0.7 kernel. Until
then, the 0.6 `map_effect` behaviour described above remains. `map_effect_par` is
built on the 0.7 bounded concurrency operators (`Concurrency::Bounded(n)`).
Callers that relied on the 0.6 concurrency switch to `map_effect_par(n, f)`; the
`m-streams` migration fragment records this.
