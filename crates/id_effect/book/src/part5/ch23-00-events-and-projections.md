# Events and Projections

> **`id_effect_events`** — published crate, tier `integration` ([ADR 0010](https://github.com/Industrial/id_effect/blob/main/docs/adrs/0010-crate-tiers.md), Amendment 6).
> Add it with `cargo add id_effect_events` (features `pg` / `sqlite` bring the `id_effect_sql` drivers).

Event sourcing keeps the **write model** as an append-only log of domain events. **Projections** fold that log into query-friendly read models. The durable store is **`SqlEventJournal`**, which runs on any `id_effect_sql` `SqlClient` (PostgreSQL or SQLite); every operation that touches a store returns an `Effect`, so nothing is appended or read until the program runs.

## What This Chapter Covers

Source lives under [`crates/id_effect_events/src`](https://github.com/Industrial/id_effect/tree/main/crates/id_effect_events/src).

- **`EventStore`** (`event_store.rs`) — append and read stream events; `append_expected` for optimistic concurrency
- **`SqlEventJournal`** (`sql_journal.rs`) — the durable journal over `SqlClient`
- **`ProjectionRunner`** (`projection_runner.rs`) — multi-projection rebuilds in dependency order
- **`MemoryEventStore`** / **`FileJournal`** (`event_store.rs`) — dev/test persistence
- **`dispatch_command`** / **`query_projection`** (`cqrs.rs`) — CQRS dispatch as effects

## Optimistic concurrency

A stream's version is its number of events. `append_expected(stream, expected_version, events)` appends only if the stream is still at `expected_version`; otherwise nothing is written and the effect fails with `EventStoreError::VersionConflict { expected, actual, .. }`. Reload the stream, re-run the command and try again.

<!-- ignore: id_effect_events is not a book_snippets dependency -->
```rust,ignore
use id_effect_events::{EventStore, EventStoreError};

let version = store.latest_version("account-1");          // Effect<u64, EventStoreError>
let append = store.append_expected("account-1", 2, &events); // fails if another writer got there first
```

## Production path (SQL journal)

<!-- ignore: id_effect_events is not a book_snippets dependency -->
```rust,ignore
use std::sync::Arc;
use id_effect_events::SqlEventJournal;
use id_effect_sql::pg::PgSqlClient;

// pool: id_effect_sql::pg::PgPool (feature `pg`); SqliteSqlClient works the same way.
let journal = SqlEventJournal::<AccountEvent>::new(Arc::new(PgSqlClient::new(pool)));
let setup = journal.create_table(); // runs SQL_EVENT_JOURNAL_DDL (or put it in a migration)
```

One table keyed by `(stream_id, version)`. An append runs in one transaction: it reads the stream's version, checks the expectation and inserts the events; a concurrent writer that takes the same version first trips the primary key, which becomes a `VersionConflict` and rolls the append back.

## ProjectionRunner

Register projection nodes with their dependencies; `plan()` orders them (ties by id), and `run_all` folds one read of the stream through each, as an effect:

<!-- ignore: id_effect_events is not a book_snippets dependency -->
```rust,ignore
use id_effect_events::{ProjectionNode, ProjectionRunner};
let mut runner = ProjectionRunner::new();
runner.register(ProjectionNode::new("summary", Vec::<&str>::new()));
runner.register(ProjectionNode::new("report", ["summary"]));
let order = runner.plan()?;
let rebuild = runner.run_all(&journal, "order-7", 1, projections_by_id);
```

## Dev stores

`MemoryEventStore` and `FileJournal` remain for unit tests and local experiments without a database. Runnable examples: `crates/id_effect_events/examples/` (`001_expected_version`, and `020_sqlite_journal` with `--features sqlite`).

See also [Async messaging](../part6/ch31-00-async-messaging.md) (outbox and obix inbox) and [Workflow and cluster](../part6/ch32-00-workflow-cluster.md) (duroxide workflow).
