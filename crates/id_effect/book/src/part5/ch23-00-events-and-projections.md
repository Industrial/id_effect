# Events and Projections

> **Labs crate — unpublished, no API guarantees ([ADR 0010](https://github.com/Industrial/id_effect/blob/main/docs/adrs/0010-crate-tiers.md)).**
> `id_effect_events` and `id_effect_graph` live in [`labs/`](https://github.com/Industrial/id_effect/tree/main/labs).
> Use them via a path or git dependency on `labs/`; they are not on crates.io. Build and test them
> with `cargo … --manifest-path labs/Cargo.toml`.

Event sourcing keeps the **write model** as an append-only log of domain events. **Projections** fold that log into query-friendly read models. Production PostgreSQL persistence uses **es-entity** on the shared `PgPool` (`id_effect_sql::pg::PgPool`, feature `pg`); multi-projection rebuild order uses **`id_effect_graph`**.

## What This Chapter Covers

Source lives under [`labs/id_effect_events/src`](https://github.com/Industrial/id_effect/tree/main/labs/id_effect_events/src) and [`labs/id_effect_graph/src`](https://github.com/Industrial/id_effect/tree/main/labs/id_effect_graph/src).

- **`EventStore`** (`event_store.rs`) — append and read stream events
- **`EsEntityEventStore`** (`es_entity/store.rs`, feature `es-entity`) — production PG journal
- **`ProjectionRunner`** (`projection_runner.rs`) — graph-ordered multi-projection rebuilds
- **`MemoryEventStore`** / **`FileJournal`** (`event_store.rs`) — dev/test persistence
- **`dispatch_command_es_entity`** (`cqrs.rs`) — async CQRS write path
- **`topological_sort`** (`id_effect_graph`) — projection dependency ordering

## Production path (es-entity)

<!-- ignore: labs crate id_effect_events is outside the workspace, not a book_snippets dependency -->
```rust,ignore
use id_effect_events::{EsEntityEventStore, EsEntityPgBackend, EventStore};
// pool: id_effect_sql::pg::PgPool
let store = EsEntityEventStore::new(EsEntityPgBackend::new(pool));
```

Apply `ES_ENTITY_EVENT_JOURNAL_DDL` at startup. See ADR [`docs/platform/adrs/adr-es-entity-event-persistence.md`](https://github.com/Industrial/id_effect/blob/main/docs/platform/adrs/adr-es-entity-event-persistence.md).

## ProjectionRunner

Register projection nodes with dependencies; `plan()` uses `id_effect_graph::topological_sort`:

<!-- ignore: labs crate id_effect_events is outside the workspace, not a book_snippets dependency -->
```rust,ignore
use id_effect_events::{ProjectionNode, ProjectionRunner};
let mut runner = ProjectionRunner::new();
runner.register(ProjectionNode::new("summary", []));
let order = runner.plan()?;
```

## Dev stores

`MemoryEventStore` and `FileJournal` remain for unit tests and local experiments without PostgreSQL.

See also [Async messaging](../part6/ch31-00-async-messaging.md) (obix outbox) and [Workflow and cluster](../part6/ch32-00-workflow-cluster.md) (duroxide workflow).
