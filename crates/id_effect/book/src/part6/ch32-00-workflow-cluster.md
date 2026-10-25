# Workflow and cluster

`id_effect_workflow` runs **durable workflows**: a business process written as one effect that
survives restarts, crashes and moves between nodes. It is a published integration-tier crate
(ADR 0010 Amendment 7) built on `id_effect_sql` (Postgres) and `id_effect_jobs`; there is no
separate workflow server. Any number of engines run against one database, so the "cluster" is
your service's replicas. (The process cluster, mailboxes and supervision, is the Process Mesh in
[Part VII](../part7/ch36-00-process-mesh.md).)

## Journal and replay

A `Workflow` has a stable `NAME`, a schema-encoded `Input` and `Output` (`#[derive(Schema)]`) and a
body `run(ctx, input) -> Effect<Output, WorkflowError>`. Everything the body does that is not
deterministic goes through its `WorkflowContext`, and each call is journaled at the next history
position:

| Call | First run | Replay |
|---|---|---|
| `ctx.step(name, effect)` | runs `effect`, journals its result | the journaled result; `effect` does not run |
| `ctx.now()`, `ctx.random()` | reads the clock / draws a number, journals it | the journaled value |
| `ctx.activity::<A>(input)` | enqueues a job, parks; journals the delivered result | the journaled result |
| `ctx.sleep(d)`, `ctx.sleep_until(t)` | enqueues a timer job at `run_at`, parks | returns at once |
| `ctx.wait_signal::<P>(name)` | takes the next stored signal or parks | the journaled signal |

When an engine resumes an instance it replays the body from the start against the history: a
journaled call returns its value without running again, and the first call past the history does
the work. A body whose calls no longer match the history (another kind or name at a position)
fails with `WorkflowError::NonDeterministic`. Steps and activities are **at least once**: one whose
result was not journaled before a crash runs again, so make its effect idempotent
(`ActivityContext::idempotency_key()` is stable across attempts).

An instance advances only under a lease. Taking it increments a fencing token that every history
append checks, so an engine that paused past its lease cannot append after another engine took the
instance over. A crashed engine's lease runs out and another engine resumes the instance from its
history.

## Activities, timers and signals

- **Activities** are the side effects: an `Activity` type runs as an `id_effect_jobs` job on any
  worker, retried by its `Schedule` up to `MAX_ATTEMPTS`; its result is delivered once per
  `(instance, seq)` and journaled. Workers get the implementation from
  `engine.register_activity(registry, activity)`.
- **Durable timers** are jobs whose `run_at` is the due time, so a wait of days survives every
  restart; with a test clock (`WallClock::anchored`, `jump`) it completes at once.
- **Signals** are stored per instance (`WorkflowClient::signal`), numbered in arrival order,
  buffered when they arrive before the wait, and wake a parked instance.

A call that waits **parks** the run: the engine frees the lease and nothing claims the instance
until its completion or signal arrives.

## Sagas

`ctx.saga(|saga| body)` runs `body` with compensations: `saga.activity::<A, C, _>(input, undo)`
runs activity `A` and registers activity `C` with `undo(&output)` as its compensation, and
`saga.compensate::<C>(input)` registers one directly. Registrations are journaled steps
(`compensate:<NAME>`), so every replay registers the same compensations with the same inputs.

<!-- ignore: id_effect_workflow is not a book_snippets dependency; the same code is crates/id_effect_workflow/examples/010_order_saga.rs -->
```rust,ignore
impl Workflow for PlaceOrder {
  type Input = Order;
  type Output = Order;
  const NAME: &'static str = "place-order";

  fn run(&self, ctx: WorkflowContext, order: Order) -> Effect<Order, WorkflowError> {
    let charge = ctx.clone();
    ctx.saga(move |saga| {
      saga
        // When a later call fails, `release-stock` runs with this reservation.
        .activity::<ReserveStock, ReleaseStock, _>(order.clone(), Reservation::clone)
        .flat_map(move |_| charge.activity::<ChargeCard>(order))
    })
  }
}
```

When the body fails with an error that fails the instance (a step or activity failure,
`WorkflowError::failed`, a decode error), the compensations run as activity calls, last registered
first, one after another, each at least once and retried by its schedule. The saga then fails with
the body's error, or with a `WorkflowError::Failed` that also names every compensation that failed
for good (the others still run). A park, a lost lease, a divergence or a store failure compensates
nothing: the run stops, and the next run replays the failure and the finished compensations from
the history and runs the rest. A rollback therefore survives restarts and takeovers like any other
part of the instance (`tests/saga_pg.rs`, `saga_compensates_in_reverse_after_restart`).

`id_effect_fsm::Saga` ([Saga and session](../part5/ch19-03-saga-session.md)) stays the in-memory
variant, for compensations that need no durability.

## Running it

```toml
[dependencies]
id_effect_workflow = "0.13"
id_effect_jobs = "0.13"
```

Apply `id_effect_workflow::migrator()` and `id_effect_jobs::migrator()`, build a
`WorkflowEngine` over a `PgWorkflowStore` with `with_job_queue(PgJobQueue::new(..))`, register the
workflows, start instances with `engine.start::<W>(id, input)` (idempotent per `WorkflowId`), and
fork `engine.run()` on every replica next to an `id_effect_jobs::Worker` serving `ACTIVITY_QUEUE`
and `TIMER_QUEUE`. `MemoryWorkflowStore` and `MemoryJobQueue` run the same engine in tests.

```bash
cargo run -p id_effect_workflow --example 010_order_saga
cargo run -p id_effect_workflow --example 020_durable_timer
DATABASE_URL=postgres://… cargo run -p id_effect_workflow --example 030_signal_approval
```

## Limits

Instances record their workflow `VERSION` but are not routed by it: changing the order or names of
an existing workflow's calls makes older instances fail their replay (versioning across code
upgrades is planned for 0.16). There is no visibility UI yet; query `workflow_instances` and
`workflow_history`, or `engine.instance(id)`.

Charter ADR: [`docs/platform/adrs/adr-workflow-charter.md`](https://github.com/Industrial/id_effect/blob/main/docs/platform/adrs/adr-workflow-charter.md).
