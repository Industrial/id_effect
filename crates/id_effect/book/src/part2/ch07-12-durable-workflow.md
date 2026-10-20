# Durable workflow (`id_effect_workflow`)

`id_effect_workflow` (published, integration tier since 0.11; ADR 0010 Amendment 7) runs a
business process as one effect that survives restarts and crashes: the workflow body is
**replayed from its history journal** whenever an engine resumes the instance. It is built on
`id_effect_sql` (PostgreSQL, `PgWorkflowStore`) and `id_effect_jobs`; `MemoryWorkflowStore` runs
the same engine in memory for tests. This chapter shows the core idea; the full model
(activities, durable timers, signals, sagas, leases and fencing) is
[Workflow and cluster](../part6/ch32-00-workflow-cluster.md).

## What problem it solves

Long-running business processes need **at-least-once** execution with **stable outputs** per
logical step. After a crash, a process should **not** repeat side effects for steps that already
completed successfully.

A `Workflow` routes its body's non-deterministic work through the `WorkflowContext`. Each call
(`ctx.step`, `ctx.now`, `ctx.random`, `ctx.activity`, `ctx.sleep`, `ctx.wait_signal`) is journaled
at the next history position; on resume a journaled call **returns the stored value** instead of
running again:

<!-- ignore: id_effect_workflow is not a book_snippets dependency; the same code is the crate-root doctest of crates/id_effect_workflow/src/lib.rs -->
```rust,ignore
use id_effect::{Effect, Schema, succeed};
use id_effect_workflow::{
  MemoryWorkflowStore, Workflow, WorkflowContext, WorkflowEngine, WorkflowError, WorkflowId,
};

#[derive(Schema)]
struct Order {
  id: i64,
}

#[derive(Schema)]
struct Receipt {
  order: i64,
  charged: i64,
}

struct Checkout;

impl Workflow for Checkout {
  type Input = Order;
  type Output = Receipt;
  const NAME: &'static str = "checkout";

  fn run(&self, ctx: WorkflowContext, order: Order) -> Effect<Receipt, WorkflowError> {
    // Journaled: after a crash the engine replays this result instead of charging again.
    let receipt = Receipt { order: order.id, charged: 42 };
    ctx.step("charge", succeed::<_, String, _>(receipt))
  }
}

let engine = WorkflowEngine::new(MemoryWorkflowStore::new()).register(Checkout)?;
let id = WorkflowId::new("checkout:7")?;
// `start` is idempotent per id; `resume` (or the `run` / `run_once` drivers) advances it.
let started = engine.start::<Checkout>(id.clone(), Order { id: 7 });
```

A body whose calls no longer match the history (another kind or name at a position) fails with
`WorkflowError::NonDeterministic`. A step whose result was not journaled before a crash runs
again on resume, so its effect must be idempotent.

## What it deliberately does *not* solve

- **Workflow versioning across code upgrades**: instances record their `VERSION` but are not
  routed by it yet (planned for 0.16); reordering or renaming calls fails older instances' replay.
- **A visibility UI**: query `workflow_instances` / `workflow_history`, or `engine.instance(id)`.
- **A separate workflow server**: the engines are your service's replicas against one database.
  For cross-organisation orchestration, **Temporal**, **Cadence** or cloud **Step Functions**
  remain options — see
  `docs/effect-ts-parity/phases/phase-g/adr-iep-g-011-temporal-vs-saga-vs-out-of-scope.md`.

## Composition with `Effect`

The engine API is effectful: `start`, `resume`, `run_once`, `run` (forever), `status` and
`result` return `Effect`s, and the body is an ordinary `Effect<Output, WorkflowError>`. Activities
and timers run as `id_effect_jobs` jobs: give the engines `with_job_queue(..)` and call
`register_activity` / `register_timers` on the workers' `JobRegistry`.

## Security and replay notes

Journaled values (inputs, outputs, step results, signal payloads) are stored as JSON and may
contain **PII** — treat them like any other database column (encryption, retention, access control
are application concerns). Details: `docs/effect-ts-parity/phases/phase-g/iep-g-012-security-replay.md`.

## Examples

```bash
cargo run -p id_effect_workflow --example 010_order_saga
cargo run -p id_effect_workflow --example 020_durable_timer
DATABASE_URL=postgres://… cargo run -p id_effect_workflow --example 030_signal_approval
```

## Semver

`id_effect_workflow` follows the integration-tier quality bar (ADR 0010); its schema ships as
versioned migrations (`id_effect_workflow::migrator()`). The unpublished
`labs/id_effect_workflow` (SQLite / duroxide step log) was removed in 0.11; see
`docs/MIGRATION-0.11.md`.
