# Durable workflow spike (`id_effect_workflow`)

This chapter describes the **experimental** labs crate `id_effect_workflow`
(`labs/id_effect_workflow`, unpublished): an **append-only** log of completed steps that
supports **single-process restart resume** — the Phase **G** spike aligned with
`docs/effect-ts-parity/phases/phase-g-cluster-workflow.md`.

## What problem it solves

Long-running business processes often need **at-least-once** execution with **stable outputs** per
logical step. After a crash, a process should **not** repeat side effects for steps that already
completed successfully.

`DurableWorkflowLog` (feature `memory`, the default; SQLite) persists each completed
`(workflow_id, seq)` with a JSON payload. On restart, `run_step_typed` **returns the stored value**
instead of invoking the closure again:

```rust,ignore
use id_effect_workflow::DurableWorkflowLog;

let mut log = DurableWorkflowLog::open(&db_path)?;
log.register_workflow("order-42")?; // once, on the first run
let charged: String = log.run_step_typed("order-42", 0, "charge-payment", || {
    Ok("payment-ok".to_string()) // runs only if step 0 is not in the log yet
})?;
```

The `duroxide` feature adds `DuroxideStepJournal` on PostgreSQL and a
`provide_duroxide_pg::<E>(pool, url)` layer providing `DuroxideProvider` and `WorkflowRuntime`.
The `fsm` module (`register_fsm`, `step_durable`, `restore_state`) persists
[state machine](../part5/ch19-00-state-machines.md) transitions as steps.

## What it deliberately does *not* solve

- **Distributed cluster execution** (no membership, no shard routing).
- **Multi-writer** correctness without external coordination (add your own leasing or use an
  orchestrator).
- **Compensation / saga policies** beyond what your application encodes in ordinary Rust.

For most production **multi-service** workflows, prefer **Temporal**, **Cadence**, or cloud
**Step Functions** — see
`docs/effect-ts-parity/phases/phase-g/adr-iep-g-011-temporal-vs-saga-vs-out-of-scope.md`.

## Composition with `Effect`

The journal API is **synchronous** (no effects). Keep it on a blocking boundary:

- From effect code, wrap a step in `sync(|| log.run_step_typed(..))` only when the program runs
  on a blocking edge (`block_on` on a plain thread), or move journal work to a dedicated thread.
- `DuroxideStepJournal`'s synchronous methods are a blocking edge themselves: called inside an
  async context (a Tokio task, `spawn_blocking`, an entered runtime) they are refused and panic
  with the `BlockOnInAsync` defect. Call them from a plain thread
  (`std::thread::spawn(move || journal.run_step_typed(..))`).

## Security and replay notes

Persisted JSON may contain **PII** — treat it like any other database column (encryption,
retention, access control are application concerns). If stored JSON is **corrupted**, replay
surfaces `WorkflowError::Json`; recovery is **policy-driven** (do not silently re-run financial
side effects). Details: `docs/effect-ts-parity/phases/phase-g/iep-g-012-security-replay.md`.

## Examples

```bash
cargo run --manifest-path labs/Cargo.toml -p id_effect_workflow --example 001_restart_resume
cargo run --manifest-path labs/Cargo.toml -p id_effect_workflow --example 030_durable_door_fsm
```

## Semver

The crate is a **labs** crate (unpublished, no guarantees); storage layout may change without
migrations until a stabilization ADR lands.
