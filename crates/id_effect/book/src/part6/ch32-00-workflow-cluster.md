# Workflow and cluster

Phase G delivers **durable workflow** without a full distributed cluster runtime.

## Production: duroxide-pg

Enable `id_effect_workflow` feature **`duroxide`**. [`DuroxideStepJournal`](../../../id_effect_workflow/struct.DuroxideStepJournal.html) persists completed steps on the shared PostgreSQL pool; duroxide-pg migrations run via [`bootstrap_duroxide_schema`](../../../id_effect_workflow/fn.bootstrap_duroxide_schema.html).

```rust
use id_effect_workflow::{DuroxideStepJournal, StepJournal, bootstrap_duroxide_schema};
bootstrap_duroxide_schema(&pool).await?;
let mut journal = DuroxideStepJournal::new(pool);
journal.register_workflow("order-42")?;
let out: String = journal.run_step_typed("order-42", 0, "validate", || Ok("ok".into()))?;
```

Register providers with [`provide_duroxide_pg`](../../../id_effect_workflow/fn.provide_duroxide_pg.html).

## Dev / tests: SQLite (`memory` feature)

[`DurableWorkflowLog`](../../../id_effect_workflow/struct.DurableWorkflowLog.html) (default `memory` feature) uses bundled SQLite for local demos.

## FSM integration

`register_fsm`, `step_durable`, and `restore_state` are generic over [`StepJournal`](../../../id_effect_workflow/trait.StepJournal.html).

```bash
cargo run -p id_effect_fsm --example 002_durable_door_fsm
# with PostgreSQL journal (requires DATABASE_URL):
cargo run -p id_effect_fsm --example 002_durable_door_fsm \
  -p id_effect_workflow --features duroxide
```

## External orchestrators

**Temporal** (and similar) remain the recommended path for multi-region cluster workflow — documented, not reimplemented in-crate.

Charter ADR: `docs/platform/adrs/adr-workflow-charter.md`.
