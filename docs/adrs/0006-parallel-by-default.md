# ADR 0006 — Parallel-by-default (Rayon)

## Status

Superseded — public `Parallelism` surface removed in favor of Fabric-only implicit dispatch ([ADR 0008](0008-implicit-parallelism.md)). Bulk parallel-by-default behavior is retained under Compute Fabric control.

Previously: **Accepted** (Rayon parallel-by-default via caller `Parallelism` policy; see decision below).

Note (2026-10-15): in 0.7, effectful concurrency is scheduled by the Fabric behind
`fork_all` / `Concurrency` ([ADR 0007 Amendment 1](0007-compute-fabric.md#amendment-1-2026-10-15-phase-c-for-07)).
The "`Effect` / `effect!` remain sequential" boundary below, already relaxed by the ADR 0008
auto-parallel binds, no longer holds: `zip_par`, `all`, `for_each_par` and the `effect!`
auto-parallel binds run effects concurrently under Fabric admission.

## Context

Commit `3e6536a` added `*_par` siblings alongside serial collection and Stream APIs. Callers must opt in to parallelism explicitly. Rayon is already a direct dependency.

## Decision

### `Parallelism` policy

- `Auto { threshold: 1024 }` — default; parallel when `len >= threshold`
- `ForceParallel` — always Rayon
- `Serial` — never Rayon

### API shape

- Primary methods (`map`, `filter`, `map_values`, `sort_with`, …) dispatch through default policy
- `*_serial` methods preserve prior serial semantics (`FnMut`, non-`Send` where applicable)
- `*_with(policy, …)` for explicit control
- `*_par` deprecated; aliases to `ForceParallel`

### Boundaries

- **`Effect` / `effect!` remain sequential** — ordering and environment borrows
- **`Stream::map_par_n`** unchanged — bounded async effect concurrency, not Rayon

## Consequences

- Default bulk ops use thread pool on large inputs
- Callers needing determinism or non-`Send` types use `*_serial`
- Breaking: primary `map`/`filter` on collections may require `Send + Sync` when input length exceeds threshold
