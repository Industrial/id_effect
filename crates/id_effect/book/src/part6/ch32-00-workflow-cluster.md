# Workflow and cluster

Phase G delivers **durable workflow** without a full distributed cluster runtime.

## `id_effect_workflow`

`DurableWorkflowLog` is an append-only SQLite journal keyed by `(workflow_id, seq)`. Completed steps replay cached JSON on restart — side effects are skipped.

```rust
use id_effect_workflow::DurableWorkflowLog;
let mut log = DurableWorkflowLog::open(path)?;
log.register_workflow("order-42")?;
let out: String = log.run_step_typed("order-42", 0, "validate", || Ok("ok".into()))?;
```

Run `cargo run -p id_effect_workflow --example 001_restart_resume`.

## Pluggable journals

`StepJournal` abstracts storage; `NetworkJournalStub` documents the distributed spike. Multi-node production should prefer Temporal or transactional outbox (`id_effect_jobs`).

## FSM integration

`register_fsm`, `step_durable`, and `restore_state` in `id_effect_fsm::workflow` persist typed snapshots. Run `cargo run -p id_effect_fsm --example 002_durable_door_fsm`.

Charter ADR: `docs/platform/adrs/adr-workflow-charter.md`.
