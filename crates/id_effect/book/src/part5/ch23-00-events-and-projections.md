# Events and Projections

> **`id_effect_events`** — published crate, tier `integration` ([ADR 0010](https://github.com/Industrial/id_effect/blob/main/docs/adrs/0010-crate-tiers.md), Amendment 6).
> Add it with `cargo add id_effect_events` (features `pg` / `sqlite` bring the `id_effect_sql` drivers).

Event sourcing keeps the **write model** as an append-only log of domain events. **Projections** fold that log into query-friendly read models. The durable store is **`SqlEventJournal`**, which runs on any `id_effect_sql` `SqlClient` (PostgreSQL or SQLite); every operation that touches a store returns an `Effect`, so nothing is appended or read until the program runs.

## What This Chapter Covers

Source lives under [`crates/id_effect_events/src`](https://github.com/Industrial/id_effect/tree/main/crates/id_effect_events/src).

- **`EventStore`** (`event_store.rs`) — append and read stream events; `append_expected` for optimistic concurrency
- **`SqlEventJournal`** (`sql_journal.rs`) — the durable journal over `SqlClient`
- **`ProjectionRunner`** (`projection_runner.rs`) — multi-projection rebuilds in dependency order
- **`SnapshotStore`** / **`load_aggregate`** (`snapshot.rs`) — aggregate snapshots: load the latest snapshot plus the events after it
- **`EventLog`** / **`CatchUpSubscription`** / **`CheckpointStore`** (`subscription.rs`) — every stream in global order, read by subscriptions that resume from a stored checkpoint
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

Each row also carries a global `position` (a unique column, `MAX(position) + 1` inside the append's transaction). A writer that takes a position blocks the next writer's insert of that position until it commits, so positions have no gaps and become visible in order: a reader that has seen position `n` never sees a smaller one turn up later. That is the property catch-up subscriptions rely on. The cost is that appends to the journal are serialized; a writer that loses the position to another stream's append is retried (`POSITION_RETRIES`) instead of being reported as a conflict.

The snapshot and checkpoint stores have their own tables, `SQL_SNAPSHOT_DDL` and `SQL_CHECKPOINT_DDL` (or `create_table()` on `SqlSnapshotStore` / `SqlCheckpointStore`). All three DDLs are the same on PostgreSQL and SQLite.

## Aggregate snapshots

An aggregate is loaded by folding its stream. A long stream makes that slow, so `load_aggregate` starts from the stream's latest `Snapshot` (the state folded up to a version) and folds only the events after it. It returns the current state as a `Snapshot` at the stream's version, so the version you pass to `append_expected` comes back with it:

<!-- ignore: id_effect_events is not a book_snippets dependency -->
```rust,ignore
use id_effect_events::{Snapshot, SnapshotStore, SqlSnapshotStore, load_aggregate};

let snapshots = SqlSnapshotStore::<Account>::new(Arc::clone(journal.client())); // state as JSON
let loaded = load_aggregate(&journal, &snapshots, "account-1", AccountFold); // Effect<Snapshot<Account>, _>
// ... decide on loaded.state, then append_expected("account-1", loaded.version, &events)
// Every N events (your policy), store the new state:
let save = snapshots.save(&Snapshot { stream_id: "account-1".into(), version, state });
```

A snapshot is a cache. Deleting one changes how much is read, never the state that is loaded. Each stream keeps one snapshot (the stream is the aggregate). Saving a snapshot older than the stored one does nothing, so a slow writer cannot move a stream's snapshot backwards. If the projection's fold changes, delete the snapshots so they are rebuilt from the events. `MemorySnapshotStore` covers tests.

## Catch-up subscriptions

A read model that spans streams (balances of every account, a search index) reads the journal in global order. `EventLog::read_all(after, limit)` returns `RecordedEvent`s (position, stream id, the stored event); `SqlEventJournal` and `MemoryEventStore` implement it. A `CatchUpSubscription` is a named reader of that log. It keeps its position in a `CheckpointStore`:

<!-- ignore: id_effect_events is not a book_snippets dependency -->
```rust,ignore
use id_effect_events::{CatchUpSubscription, RecordedEvent, SqlCheckpointStore};

let checkpoints = SqlCheckpointStore::new(Arc::clone(journal.client()));
let balances = CatchUpSubscription::new("balances", journal.clone(), checkpoints);
let apply = move |e: &RecordedEvent<AccountEvent>| -> Effect<(), SqlError> {
  // update the read model, ignoring positions it has already applied
};
let caught_up = balances.catch_up(apply); // Effect<usize, SubscriptionError<SqlError>>
```

`poll` handles one batch after the checkpoint and then stores the last position. `catch_up` polls until a batch comes back short. A restarted process builds the subscription with the same name and continues after the stored checkpoint. Run `catch_up` at startup, then on a schedule or after appends.

Delivery is at least once. The checkpoint is stored after each batch, so a process that stops between handling a batch and storing the checkpoint sees that batch again. When the handler fails, the checkpoint is stored at the event before the failure and the effect fails with `SubscriptionError::Handler { position, .. }`. Make handlers idempotent: keep the last applied position with the read model, as `examples/010_account.rs` does. Then a repeated event changes nothing. Keep the checkpoint table in the read model's database, so a restore brings both back together.

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

`MemoryEventStore` and `FileJournal` remain for unit tests and local experiments without a database. Runnable examples: `crates/id_effect_events/examples/`: `001_expected_version`, and with `--features sqlite` `010_account` (snapshots and a checkpointed balances projection across a restart) and `020_sqlite_journal`.

See also [Async messaging](../part6/ch31-00-async-messaging.md) (outbox and obix inbox) and [Workflow and cluster](../part6/ch32-00-workflow-cluster.md) (duroxide workflow).
