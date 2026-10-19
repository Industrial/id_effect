# `id_effect_workflow`

Durable workflows for id_effect services (ADR 0010 Amendment 7): a workflow body is replayed from
its history journal whenever an engine resumes the instance, on `id_effect_sql` (Postgres) with any
number of engines against one database. Steps, `now()` and `random()` are journaled; activities
and durable timers run as `id_effect_jobs` jobs on workers of any node; signals are stored and
delivered in order; sagas register compensations that run as journaled activities, in reverse
order, when the body fails. Instances advance only under a lease whose fencing token is checked on
every history append. `MemoryWorkflowStore` runs the same engine in memory for tests.

```rust
use id_effect::{Effect, Schema, succeed};
use id_effect_workflow::{
  Activity, ActivityContext, JobFailure, Workflow, WorkflowContext, WorkflowError,
};

#[derive(Clone, Schema)]
struct Order {
  id: i64,
}

struct Reserve;

impl Activity for Reserve {
  type Input = Order;
  type Output = Order;
  const NAME: &'static str = "reserve";

  fn run(&self, _: ActivityContext, order: Order) -> Effect<Order, JobFailure> {
    succeed(order)
  }
}

struct Release;

impl Activity for Release {
  type Input = Order;
  type Output = Order;
  const NAME: &'static str = "release";

  fn run(&self, _: ActivityContext, order: Order) -> Effect<Order, JobFailure> {
    succeed(order)
  }
}

struct Charge;

impl Activity for Charge {
  type Input = Order;
  type Output = Order;
  const NAME: &'static str = "charge";

  fn run(&self, _: ActivityContext, order: Order) -> Effect<Order, JobFailure> {
    succeed(order)
  }
}

struct PlaceOrder;

impl Workflow for PlaceOrder {
  type Input = Order;
  type Output = Order;
  const NAME: &'static str = "place-order";

  fn run(&self, ctx: WorkflowContext, order: Order) -> Effect<Order, WorkflowError> {
    let charge = ctx.clone();
    // If `charge` fails for good, `release` runs (a journaled activity) before the instance fails.
    ctx.saga(move |saga| {
      saga
        .activity::<Reserve, Release, _>(order.clone(), Order::clone)
        .flat_map(move |_| charge.activity::<Charge>(order))
    })
  }
}
```

Examples:

- `cargo run -p id_effect_workflow --example 010_order_saga`: a saga whose declined charge
  releases the reservation (in memory).
- `cargo run -p id_effect_workflow --example 020_durable_timer`: a day-long timer, fired under a
  test clock and finished by another engine (in memory).
- `DATABASE_URL=postgres://… cargo run -p id_effect_workflow --example 030_signal_approval`: a
  signal sent by a separate client wakes a parked instance (memory without `DATABASE_URL`).

The design (journal and replay, parking, activities, timers, signals, sagas, schema, leases and
fencing) is in the crate documentation. The live tests (`tests/*_pg.rs`) need `DATABASE_URL`;
`ID_EFFECT_REQUIRE_PG=1` makes a missing database a failure. Book: "Workflows"
(`crates/id_effect/book/src/part6/ch32-00-workflow-cluster.md`).
