# `id_effect_workflow`

**Experimental (0.1.x):** append-only SQLite step log with **resume** semantics for single-process (or single-writer) workflows. This crate implements **Phase G** of `docs/effect-ts-parity/phases/phase-g-cluster-workflow.md` — a deliberate **narrow** slice compared to `@effect/cluster` / `@effect/workflow` or systems like Temporal.

## Semver policy

- **0.1.x:** API may evolve quickly; storage layout may change without migration tooling.
- A future **0.2** or **1.0** would only follow after `iep-g-022` style retrospective and explicit stabilization.

## When to use this vs an external orchestrator

See the mdBook chapter **“Durable workflow spike (`id_effect_workflow`)”** (`crates/id_effect/book/src/part2/ch07-12-durable-workflow.md`) for trade-offs: durability scope, multi-writer limits, and when to adopt Temporal / Step Functions instead.

## Composition with `id_effect`

The log API is **synchronous** (Rusqlite). Run it **inside** `effect!` blocks or behind `from_async` + `spawn_blocking` at your application boundary — see tests in `src/lib.rs` and the book chapter.
