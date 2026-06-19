# Durable workflow spike (`id_effect_workflow`)

This chapter describes the **experimental** `id_effect_workflow` crate: a **SQLite-backed**, **append-only** log of completed steps that supports **single-process restart resume** — the Phase **G** spike aligned with `docs/effect-ts-parity/phases/phase-g-cluster-workflow.md`.

## What problem it solves

Long-running business processes often need **at-least-once** execution with **stable outputs** per logical step. After a crash, a process should **not** repeat side effects for steps that already completed successfully.

`DurableWorkflowLog` persists each completed `(workflow_id, seq)` with a JSON payload. On restart, `run_step_typed` **returns the stored value** instead of invoking the closure again.

## What it deliberately does *not* solve

- **Distributed cluster execution** (no membership, no shard routing).
- **Multi-writer** correctness without external coordination (add your own leasing or use an orchestrator).
- **Compensation / saga policies** beyond what your application encodes in ordinary Rust.

For most production **multi-service** workflows, prefer **Temporal**, **Cadence**, or cloud **Step Functions** — see `docs/effect-ts-parity/phases/phase-g/adr-iep-g-011-temporal-vs-saga-vs-out-of-scope.md`.

## Composition with `Effect`

The log API is **synchronous** (Rusqlite). Keep SQLite on a **blocking** boundary:

- **Inside `effect!`:** perform log IO directly when your interpreter runs on a blocking runtime (see unit tests in `id_effect_workflow`).
- **Async hosts:** wrap calls in `from_async` + `spawn_blocking` (or your platform’s blocking pool) so you do not block async executors.

## Security and replay notes

Persisted JSON may contain **PII** — treat it like any other database column (encryption, retention, access control are application concerns). If stored JSON is **corrupted**, replay surfaces `WorkflowError::Json`; recovery is **policy-driven** (do not silently re-run financial side effects). Details: `docs/effect-ts-parity/phases/phase-g/iep-g-012-security-replay.md`.

## Semver

The crate ships at **0.1.x** as **experimental**; storage layout may change without migrations until a stabilization ADR lands.
