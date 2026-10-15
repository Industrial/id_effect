# Workflow and cluster

> **Labs crate — unpublished, no API guarantees ([ADR 0010](https://github.com/Industrial/id_effect/blob/main/docs/adrs/0010-crate-tiers.md)).**
> `id_effect_workflow` lives in [`labs/id_effect_workflow`](https://github.com/Industrial/id_effect/tree/main/labs/id_effect_workflow).
> Use it via a path or git dependency on `labs/`; it is not on crates.io. The published cluster
> runtime is the Process Mesh (`id_effect_process` + `id_effect_node`, [Part VII](../part7/ch36-00-process-mesh.md)).

Phase G delivers **durable workflow** without a full distributed cluster runtime.

## Production: duroxide-pg

Enable `id_effect_workflow` feature **`duroxide`**. `DuroxideStepJournal` persists completed steps on the shared PostgreSQL pool; duroxide-pg migrations run via `bootstrap_duroxide_schema`.

```rust,ignore
use id_effect_workflow::{DuroxideStepJournal, StepJournal, bootstrap_duroxide_schema};
bootstrap_duroxide_schema(&pool).await?;
let mut journal = DuroxideStepJournal::new(pool);
journal.register_workflow("order-42")?;
let out: String = journal.run_step_typed("order-42", 0, "validate", || Ok("ok".into()))?;
```

The journal API is synchronous. To give effects the duroxide capabilities, provide the layer
`provide_duroxide_pg::<E>(pool, url)` (`Layer<HCons<DuroxideProvider, HCons<WorkflowRuntime, HNil>>, E>`;
the caller owns the pool): `program.provide_layer(provide_duroxide_pg::<AppError>(pool, url))`.

## Dev / tests: SQLite (`memory` feature)

`DurableWorkflowLog` (default `memory` feature) uses bundled SQLite for local demos.

## FSM integration

`id_effect_workflow::fsm::{register_fsm, step_durable, restore_state}` are generic over `StepJournal`. They moved here from `id_effect_fsm` in 0.6, so the published fsm crate has no labs dependency. Run the example from the labs workspace:

```bash
cargo run --manifest-path labs/Cargo.toml -p id_effect_workflow --example 030_durable_door_fsm
cargo run --manifest-path labs/Cargo.toml -p id_effect_workflow --example 001_restart_resume
```

## External orchestrators

**Temporal** (and similar) remain the recommended path for multi-region cluster workflow — documented, not reimplemented in-crate.

Charter ADR: [`docs/platform/adrs/adr-workflow-charter.md`](https://github.com/Industrial/id_effect/blob/main/docs/platform/adrs/adr-workflow-charter.md).
