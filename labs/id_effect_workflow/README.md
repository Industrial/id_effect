# `id_effect_workflow`

**Experimental (0.1.x):** append-only SQLite step log with **resume** semantics for single-process (or single-writer) workflows. This crate implements **Phase G** of `docs/effect-ts-parity/phases/phase-g-cluster-workflow.md` — a deliberate **narrow** slice compared to `@effect/cluster` / `@effect/workflow` or systems like Temporal.

## Semver policy

- **0.1.x:** API may evolve quickly; storage layout may change without migration tooling.
- A future **0.2** or **1.0** would only follow after `iep-g-022` style retrospective and explicit stabilization.

## When to use this vs an external orchestrator

See the mdBook chapter **“Durable workflow spike (`id_effect_workflow`)”** (`crates/id_effect/book/src/part2/ch07-12-durable-workflow.md`) for trade-offs: durability scope, multi-writer limits, and when to adopt Temporal / Step Functions instead.

## Composition with `id_effect`

The log API is **synchronous** (Rusqlite). Run it **inside** `id_effect::effect!` blocks (the `?` operator lifts its `Result`s) or on a thread at your application boundary — see the tests in `src/sqlite_log.rs` / `src/fsm.rs` and the book chapter.

The duroxide (PostgreSQL) `StepJournal` is a synchronous **blocking edge** over async sqlx: call it from a plain thread, not from inside a tokio task (there it panics with `BlockOnInAsync`). See `docs/migration-0.7/l-labs-durable.md`.
