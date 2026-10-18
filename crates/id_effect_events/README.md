# id_effect_events

Event sourcing, projections, and CQRS boundaries for [`id_effect`](https://github.com/Industrial/id_effect) programs
(tier `integration`, ADR 0010 Amendment 6).

- **`EventStore`** — append-only streams with per-stream versions; `append_expected(stream, expected_version, events)`
  fails with `EventStoreError::VersionConflict` when another writer got there first
- **`SqlEventJournal`** — the durable store, over any `id_effect_sql` `SqlClient` (PostgreSQL, SQLite: features `pg`,
  `sqlite`); `MemoryEventStore` and `FileJournal` for tests and local runs
- **`Projection`** / **`run_projection_from_store`** / **`ProjectionRunner`** — fold streams into read models, several
  in dependency order
- **`SnapshotStore`** / **`load_aggregate`** — aggregate snapshots: load the latest snapshot plus the events after it
  (`SqlSnapshotStore`, `MemorySnapshotStore`)
- **`EventLog`** / **`CatchUpSubscription`** — every stream in global position order; subscriptions resume from a
  checkpoint in a `CheckpointStore` (`SqlCheckpointStore`, `MemoryCheckpointStore`), at least once
- **`CommandHandler`** / **`QueryHandler`** / **`dispatch_command`** — CQRS dispatch, as effects
- **`EventEnvelope`** — metadata shell with `Schema` wire bridging

```rust
use id_effect::{Effect, Exit, fail, succeed};
use id_effect_events::{CommandHandler, MemoryEventStore, Projection, dispatch_command};

#[derive(Clone, Debug, PartialEq)]
enum Event {
  Deposited(i64),
}

struct Deposit(i64);

#[derive(Debug)]
struct Rejected;
impl std::fmt::Display for Rejected {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str("deposits must be positive")
  }
}
impl std::error::Error for Rejected {}

struct Accounts;
impl CommandHandler<Deposit, Event> for Accounts {
  type Error = Rejected;
  fn handle(&self, Deposit(amount): Deposit) -> Effect<Vec<Event>, Rejected> {
    if amount <= 0 { fail(Rejected) } else { succeed(vec![Event::Deposited(amount)]) }
  }
}

struct Balance;
impl Projection<i64, Event> for Balance {
  fn initial(&self) -> i64 {
    0
  }
  fn apply(&self, balance: i64, event: &Event) -> i64 {
    match event {
      Event::Deposited(n) => balance + n,
    }
  }
}

let store = MemoryEventStore::new();
// An effect: nothing is handled or stored until it runs.
let deposit = dispatch_command(&Accounts, &store, "account-1", Balance, Deposit(25));
assert!(matches!(deposit.block_on(), Exit::Success(25)));
```

See the mdBook chapter *Events and projections* (`part5/ch23-00-events-and-projections.md`) and `examples/`.
