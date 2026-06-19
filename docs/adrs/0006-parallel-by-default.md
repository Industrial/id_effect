# ADR 0006 — Parallel-by-default (Rayon)

## Status

Accepted

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
