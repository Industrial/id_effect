# Messaging (`id_effect_messaging`)

`id_effect_messaging` publishes what happened — `order.placed`, `invoice.paid` — to topics that any number of consumer groups read. It defines the broker-agnostic `MessageBroker`, ships `MemoryBroker` for tests and single-process services, and makes publishing reliable on PostgreSQL with a transactional outbox, a relay and an idempotent inbox.

```toml
id_effect_messaging = "0.13"
```

## Topics, messages and consumer groups

A `Topic` is a validated name: 1 to 249 ASCII letters, digits, `.`, `_` or `-`, which every supported broker accepts (`Topic::new("a b")` is a `TopicError`). A `Message` is a payload with an id (`MessageId`, generated unless set with `with_id`), an optional partition `key` and headers; `Message::encode(value)` / `message.decode::<T>()` go through a `#[derive(Schema)]` type.

Every **consumer group** subscribed to a topic sees every message published to it; the consumers of one group share them. A group that does not exist yet starts at `StartFrom::Earliest` (default) or `Latest`.

## Subscribing and settling

`broker.subscribe(&topic, &group, options)` completes once the group exists and yields a `Stream` of `Delivery`s. Each delivery is settled once:

| Settle with | Then |
|---|---|
| `delivery.ack()` | done for this group |
| `delivery.nack(Nack::Requeue)` (or drop it unsettled) | delivered again, until its `max_deliveries`-th delivery, then moved to the dead-letter topic |
| `delivery.nack(Nack::DeadLetter)` | dead-lettered now (a poison message) |

`SubscribeOptions` sets `prefetch` (at most this many unsettled deliveries per subscription, default 16), `concurrency` (handlers run at once by `consume`, default 1), `max_deliveries` (default 5), `dead_letter` (default `<topic>.dlq`) and `start`. A dead-lettered message keeps its payload and gains headers with the reason (`DeadLetterReason::MaxDeliveries` or `Rejected`).

`broker.consume(&topic, &group, options, handler)` runs `handler` over the deliveries until the stream ends or fails; `ack_on_success(f)` is the usual handler: ack when `f` succeeds, requeue when it fails. Delivery is at least once, so handlers must be idempotent — see the inbox below.

```rust
use id_effect_messaging::{
  ConsumerGroup, MemoryBroker, Message, MessageBroker, Nack, SubscribeOptions, Topic,
};

let broker = MemoryBroker::new();
let orders = Topic::new("orders").unwrap();
let billing = ConsumerGroup::new("billing").unwrap();
let opts = SubscribeOptions::default().max_deliveries(2);
let eff = broker
  .publish(&orders, Message::new("order 1"))
  .flat_map({
    let (broker, orders, billing) = (broker.clone(), orders.clone(), billing.clone());
    move |()| broker.subscribe(&orders, &billing, opts)
  })
  .flat_map(|deliveries| deliveries.take(2).run_for_each(|d| d.nack(Nack::Requeue)));
assert!(eff.block_on().into_result().is_ok());
// Delivered twice, nacked twice: dead-lettered.
let dlq = broker.messages(&Topic::new("orders.dlq").unwrap());
assert_eq!(dlq[0].payload(), b"order 1");
```

## Brokers

| Broker | Crate | For |
|---|---|---|
| `MemoryBroker` | `id_effect_messaging` | tests and single-process services: every semantic of the trait in memory; `broker.messages(&topic)` shows what was published |
| Redis Streams | `id_effect_redis`, feature `streams` | consumer groups on Redis Streams |

Code written against `MessageBroker` (or `Arc<dyn MessageBroker>`) runs on either.

## The transactional outbox

Publishing from a request after its transaction commits loses the message when the process dies in between; publishing before the commit sends a message for work that may roll back. The outbox closes the gap:

1. `Outbox::new().stage(&tx, &topic, message)` writes the message to `id_effect_outbox` **with the caller's transaction**: it exists if and only if the caller's work commits.
2. A `Relay` claims committed rows from `PgOutbox` (`FOR UPDATE SKIP LOCKED` with a lease, so several relays share one outbox), publishes each through any `MessageBroker` with the id it was staged with, and marks it sent. A failed publish is retried on a core `Schedule` (`RelayConfig::retry`); once a finite schedule is done the row is kept as dead, never published. Sent rows are pruned after `prune_after` (default 1 day).
3. A relay that crashes after publishing publishes again, so the broker may carry a message twice — with the same `MessageId`.

`relay.run()` runs until interrupted; `relay.run_until_idle()` publishes everything due and returns a `RelayReport`, for tests and batch programs.

## The idempotent inbox

`Inbox::new().once(&tx, &group, &message_id, f)` records `(consumer group, message id)` in `id_effect_inbox` with the handler's transaction and runs `f` only the first time (`Once::Ran(a)`; later `Once::Duplicate`). The record commits or rolls back with `f`'s work, so a failed message is processed again on its redelivery. `Inbox::handler(client, f)` is the `consume` handler built on it: it begins a transaction per delivery, runs `f` through `once`, commits, then acks — a duplicate is acked without running `f`, a failure rolls back and requeues. `inbox.prune(&client, keep)` deletes old rows; keep them well beyond the longest redelivery and outbox-retry horizon.

```rust,no_run
use std::sync::Arc;

use id_effect::{Effect, Never};
use id_effect_messaging::{
  BrokerError, ConsumerGroup, Inbox, Message, MessageBroker, MessageBrokerExt, Outbox, PgOutbox,
  Relay, SubscribeOptions, Topic,
};
use id_effect_sql::pg::PgSqlClient;
use id_effect_sql::{SqlClient, SqlError, SqlTransaction, params, with_transaction};

fn placed() -> Topic {
  Topic::new("order.placed").expect("a valid topic")
}

/// The order and its message commit together.
fn place_order(client: Arc<dyn SqlClient>, order: i64) -> Effect<(), SqlError> {
  with_transaction(client, move |tx: Arc<dyn SqlTransaction>| {
    tx.execute("INSERT INTO orders (id) VALUES ($1)", &params![order])
      .flat_map(move |_| {
        let message = Message::new(order.to_string()).with_id(format!("order-{order}"));
        Outbox::new().stage(&tx, &placed(), message)
      })
      .map(|_| ())
  })
}

/// Publishes the committed messages until interrupted.
fn relay(client: PgSqlClient, broker: Arc<dyn MessageBroker>) -> Effect<(), Never> {
  Relay::new(Arc::new(PgOutbox::new(client)), broker).run()
}

/// Billing invoices each order once, however often it is delivered.
fn billing(client: Arc<dyn SqlClient>, broker: Arc<dyn MessageBroker>) -> Effect<(), BrokerError> {
  let group = ConsumerGroup::new("billing").expect("a valid group");
  let handler = Inbox::new().handler(client, |tx: Arc<dyn SqlTransaction>, message: &Message| {
    let order = message.text().unwrap_or_default().to_owned();
    tx.execute("INSERT INTO invoices (order_id) VALUES ($1)", &params![order])
      .map(|_| ())
  });
  broker.consume(&placed(), &group, SubscribeOptions::default().concurrency(4), handler)
}
# fn main() {}
```

`MemoryOutbox` and `MemoryInbox` are the same in memory (`MemoryOutbox::stage(&topic, message)` needs no transaction). The crate's `migrator()` creates `id_effect_outbox` and `id_effect_inbox`, tracked in `_id_effect_messaging_migrations`; `PgOutbox::ensure_schema()` creates the outbox idempotently.

## Examples

```bash
cargo run -p id_effect_messaging --example 010_topics
cargo run -p id_effect_messaging --example 020_memory_broker   # groups, redelivery, dead letters
cargo run -p id_effect_messaging --example 030_outbox_inbox    # stage, relay, deduplicate
```

The PostgreSQL tests (`tests/pg_outbox.rs`) run with `DATABASE_URL` set; `ID_EFFECT_REQUIRE_PG=1` makes a missing database a failure.
