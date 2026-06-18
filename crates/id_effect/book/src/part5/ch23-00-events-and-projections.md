# Events and Projections

Event sourcing keeps the **write model** as an append-only log of domain events. **Projections** fold that log into query-friendly read models. This chapter introduces `id_effect_events` for CQRS boundaries and `id_effect_graph` for dependency ordering.

## What This Chapter Covers

- **[`EventStore`](../../id_effect_events/src/event_store.rs)** — append and read stream events
- **[`MemoryEventStore`](../../id_effect_events/src/event_store.rs)** / **[`FileJournal`](../../id_effect_events/src/event_store.rs)** — in-memory and JSON-lines persistence
- **[`EventEnvelope`](../../id_effect_events/src/envelope.rs)** — metadata shell with [`Schema`](../../src/schema/parse.rs) wire bridging
- **[`run_projection`](../../id_effect_events/src/projection.rs)** — fold events into read models
- **[`CommandHandler`](../../id_effect_events/src/cqrs.rs)** / **[`QueryHandler`](../../id_effect_events/src/cqrs.rs)** — CQRS dispatch
- **[`Dag`](../../id_effect_graph/src/dag.rs)** / **[`topological_sort`](../../id_effect_graph/src/topological_sort.rs)** — explicit and capability-style dependency graphs

## EventStore

An [`EventStore`] assigns monotonic versions per stream. Append is the only mutation; reads are slice queries from a version:

```rust
use id_effect::{run_blocking, Effect};
use id_effect_events::{EventStore, MemoryEventStore};

#[derive(Clone)]
enum AccountEvt { Deposited(i32) }

let store = MemoryEventStore::<AccountEvt>::new();
let stored = run_blocking(
  store.append("acct-1", &[AccountEvt::Deposited(100)]),
  (),
).expect("append");
assert_eq!(stored[0].version, 1);
```

[`FileJournal`] persists the same API to a JSON-lines file — useful for local spikes and integration tests.

## EventEnvelope and Schema

Wrap payloads with stream metadata and encode through [`HasSchema`](../../src/schema/has_schema.rs):

```rust
use id_effect::schema::HasSchema;
use id_effect_events::EventEnvelope;

struct Deposited;
impl HasSchema for Deposited {
  type A = i64;
  type I = i64;
  type E = ();
  fn schema() -> id_effect::Schema<Self::A, Self::I, Self::E> {
    id_effect::schema::i64::<()>()
  }
}

let env = EventEnvelope::new("acct-1", 1, "Deposited", 50_i64);
let wire = env.to_wire::<Deposited>().expect("wire");
```

Use [`envelope_schema`](../../id_effect_events/src/envelope.rs) when you need a composite [`Schema`] for the full envelope.

## Projections

Implement [`Projection`] with `initial` and `apply`, then fold:

```rust
use id_effect_events::{Projection, run_projection, run_projection_from_store};

struct Balance;
impl Projection<i32, AccountEvt> for Balance {
  fn initial(&self) -> i32 { 0 }
  fn apply(&self, state: i32, event: &AccountEvt) -> i32 {
    match event { AccountEvt::Deposited(n) => state + n }
  }
}

let total = run_projection(&Balance, events);
let from_store = run_blocking(
  run_projection_from_store(&store, "acct-1", 1, &Balance),
  (),
).expect("project");
```

Pair projections with [`SubscriptionRef`](../../src/coordination/subscription_ref.rs) when subscribers need live read-model updates (see ch21).

## CQRS dispatch

[`CommandHandler`] validates commands and returns events; [`dispatch_command`] persists them and returns the updated projection:

```rust
use id_effect_events::{CommandHandler, dispatch_command, Effect};

struct Deposit { amount: i32 }
struct DepositHandler;
impl CommandHandler<Deposit, AccountEvt> for DepositHandler {
  type Error = &'static str;
  fn handle(&self, cmd: Deposit) -> Effect<Vec<AccountEvt>, Self::Error, ()> {
    if cmd.amount <= 0 { return Effect::fail("invalid"); }
    Effect::succeed(vec![AccountEvt::Deposited(cmd.amount)])
  }
}

let state = run_blocking(
  dispatch_command(&DepositHandler, &store, "acct-1", &Balance, Deposit { amount: 10 }),
  (),
).expect("dispatch");
```

[`QueryHandler`] answers from materialized state; use it at API boundaries after rebuilding or caching projections.

## id_effect_graph

Capability provider graphs use the same topological sort as [`plan_topological`](../../src/capability/planner.rs). The standalone crate exposes:

| API | Use |
|-----|-----|
| [`Dag::add_edge`](../../id_effect_graph/src/dag.rs) | Explicit `dependency → dependent` edges |
| [`DependencyNode`](../../id_effect_graph/src/topological_sort.rs) | `requires` / `provides` resolution |
| [`topological_sort`](../../id_effect_graph/src/topological_sort.rs) | Build order for named providers |

```rust
use id_effect_graph::{Dag, DependencyNode, topological_sort};

let mut dag = Dag::new();
dag.add_node("db").unwrap();
dag.add_node("repo").unwrap();
dag.add_edge("db", "repo").unwrap();
let order = dag.sort().unwrap();
```

Use graphs for event-handler registration order, saga step wiring, or any DAG constraint — not only capability DI.
