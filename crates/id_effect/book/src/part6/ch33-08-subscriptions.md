# Subscriptions over the event log

A subscription is a stream a client can pick up again where it left off. Every item carries a
**cursor**, and a client that reconnects sends the last cursor it saw; the server replays from
there. When the items come from an `id_effect_events` log, the cursor is simply the event's
global position, and `id_effect_rpc::subscription::events::EventSubscription` (feature `events`)
does the rest: replay from the cursor, then follow the live tail.

```rust
use id_effect::{DynEnv, fail, schema, succeed};
use id_effect_events::{EventStore, MemoryEventStore};
use id_effect_rpc::subscription::TailConfig;
use id_effect_rpc::subscription::events::EventSubscription;
use id_effect_rpc::wire::Cursor;
use id_effect_rpc::{Procedure, RpcGroup, WireSchema};
use std::time::Duration;

let store = MemoryEventStore::<i64>::new(); // or a `SqlEventJournal` on PostgreSQL
let entries = EventSubscription::new(store.clone())
  .with_config(TailConfig::new().with_batch_size(100).with_poll_interval(Duration::from_millis(250)));

let mut group = RpcGroup::new();
group.subscription(
  Procedure::subscription("Ledger.entries").public(),
  WireSchema::new(schema::string::<()>()), // the account to follow
  WireSchema::new(schema::i64::<()>()),    // one amount per item
  move |account: String, after: Option<Cursor>, _env: &mut DynEnv| {
    // The projection keeps this account's events; the item's cursor is the event's position.
    let project = move |e: id_effect_events::RecordedEvent<i64>| {
      (e.stream_id == account).then_some(e.event.payload)
    };
    match entries.subscribe(after, project) {
      Ok(subscription) => succeed(subscription),
      Err(mismatch) => fail(mismatch), // `{"_tag": "CursorMismatch"}`, 400
    }
  },
);
# let _ = store.append("acct-1", &[5]).block_on();
```

## What a call does

- **Cursor = position.** Items carry `Cursor::events(position)`. On the wire it is the opaque
  15-character `c1.…` string: the SSE `id:` field (a reconnecting `EventSource` sends it back as
  `Last-Event-ID`), or the `cursor` of WebSocket `item` frames and of the `resume` frame.
- **Replay, then tail, from one position.** The reader asks the log for the events after the
  cursor, delivers them, and asks again after the last one. When a read finds nothing new it
  waits the poll interval and asks again. History and live tail are the same loop, so nothing
  falls between them, and a resume from the last cursor neither skips nor repeats an event.
- **Projection.** The function you pass sees every event in order and keeps the ones the
  subscriber should get. Dropped events still move the position forward.
- **No cursor** starts after `starting_after(n)`: `0` (the default) replays the whole log; the
  log's head means "only what happens from now on".

## Rules that keep it safe

- **A cursor is not a capability.** It names a position, not a right. A resume is a new call,
  so the procedure's guards run again, and the projection is applied again — a cursor from
  someone else's session shows nothing the caller could not already subscribe to. A cursor of
  another kind (a stream's sequence cursor) is refused with `CursorMismatch`.
- **Bounded memory, no unbounded buffer.** The reader only reads when the transport pulls.
  A slow subscriber fills its window (the transport's credit window, 32 items on frame
  transports) and its reader stops; the backlog stays in the log, not in the server. When the
  subscriber reads again it catches up from its own position. One call holds at most the window
  plus one read batch; `TailConfig` clamps the batch to 1–1,000 events and the poll interval to
  10 ms–60 s.
- **Interrupts release the reader.** A disconnect, a `cancel` frame or the deadline drops the
  call; the fiber that reads (also one waiting out its poll interval) is interrupted.
  `EventSubscription::open_readers()` counts the readers still alive — useful in tests and as a
  gauge.
- **Faults stay on the server.** A failed read ends the call as a defect: it is logged with the
  correlation id and the client gets `InternalError { id }`, never the database's message. The
  client resumes from its last cursor.

## Testing it

The crate's `tests/subscription_events.rs` runs the same checks against the in-memory store and
against PostgreSQL in a container the test starts itself (`id_effect_test::Shared<Postgres>`),
never the `DATABASE_URL` of the shell:

```text
cargo nextest run -p id_effect_rpc --features events
ID_EFFECT_REQUIRE_PG=1 cargo nextest run -p id_effect_rpc --features events --test subscription_events
```

Without Docker the PostgreSQL tests print `SKIP`; with `ID_EFFECT_REQUIRE_PG=1` they fail instead.
