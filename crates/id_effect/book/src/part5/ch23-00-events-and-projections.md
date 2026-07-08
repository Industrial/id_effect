# Events and Projections

Event sourcing keeps the **write model** as an append-only log of domain events. **Projections** fold that log into query-friendly read models. Production PostgreSQL persistence uses **es-entity** on the shared [`PgPool`](../../id_effect_sql_pg/struct.PgPool.html); multi-projection rebuild order uses **`id_effect_graph`**.

## What This Chapter Covers

- **[`EventStore`](../../id_effect_events/src/event_store.rs)** — append and read stream events
- **[`EsEntityEventStore`](../../id_effect_events/src/es_entity/store.rs)** (feature `es-entity`) — production PG journal
- **[`ProjectionRunner`](../../id_effect_events/src/projection_runner.rs)** — graph-ordered multi-projection rebuilds
- **[`MemoryEventStore`](../../id_effect_events/src/event_store.rs)** / **[`FileJournal`](../../id_effect_events/src/event_store.rs)** — dev/test persistence
- **[`dispatch_command_es_entity`](../../id_effect_events/src/cqrs.rs)** — async CQRS write path
- **[`topological_sort`](../../id_effect_graph/src/topological_sort.rs)** — projection dependency ordering

## Production path (es-entity)

```rust
use id_effect_events::{EsEntityEventStore, EsEntityPgBackend, EventStore};
// pool from id_effect_sql_pg::PgPool
let store = EsEntityEventStore::new(EsEntityPgBackend::new(pool));
```

Apply [`ES_ENTITY_EVENT_JOURNAL_DDL`](../../id_effect_events/constant.ES_ENTITY_EVENT_JOURNAL_DDL.html) at startup. See ADR `docs/platform/adrs/adr-es-entity-event-persistence.md`.

## ProjectionRunner

Register projection nodes with dependencies; `plan()` uses `id_effect_graph::topological_sort`:

```rust
use id_effect_events::{ProjectionNode, ProjectionRunner};
let mut runner = ProjectionRunner::new();
runner.register(ProjectionNode::new("summary", []));
let order = runner.plan()?;
```

## Dev stores

[`MemoryEventStore`] and [`FileJournal`] remain for unit tests and local spikes without PostgreSQL.

See also Part VI ch31 (obix outbox) and ch32 (duroxide workflow).
