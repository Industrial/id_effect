# ADR 0008 — Implicit parallelism (API collapse)

## Status

Accepted

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
