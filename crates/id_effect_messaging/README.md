# `id_effect_messaging`

Messaging for id_effect services: the broker-agnostic `MessageBroker` (publish, subscribe, ack), an
in-memory broker, and the transactional outbox, inbox and relay on `id_effect_sql`. Redis Streams
implement the broker in `id_effect_redis` (feature `streams`) (ADR 0010 Amendment 6).

`Topic` is the validated name messages are published to: 1 to 249 ASCII letters, digits, `.`, `_`
or `-`, a name every supported broker accepts.

```rust
use id_effect_messaging::{Topic, TopicError};

let topic = Topic::new("ledger.entry-posted").unwrap();
assert_eq!(topic.as_str(), "ledger.entry-posted");
assert_eq!(Topic::new(""), Err(TopicError::Empty));
```

`MessageBroker` is one trait for every broker: `publish(topic, Message)`, and `subscribe(topic,
group, SubscribeOptions { prefetch, concurrency, max_deliveries, dead_letter, start })` returning a
`Stream` of `Delivery`s, each settled once with `ack()` or `nack(Nack::Requeue | Nack::DeadLetter)`.
Every consumer group gets every message; the subscriptions of a group share them. A requeued (or
dropped) delivery comes again until `max_deliveries`, then moves to the dead-letter topic
(`<topic>.dlq` by default) with `x-dead-letter-*` headers; at most `prefetch` deliveries of a
subscription are unsettled at once, and `consume` runs `concurrency` handlers at a time.
`MemoryBroker` implements all of it in memory for tests; `Message::encode` / `decode` carry
`Schema`-typed payloads as JSON.

```rust
use id_effect_messaging::{
  ConsumerGroup, MemoryBroker, Message, MessageBroker, SubscribeOptions, Topic,
};

let broker = MemoryBroker::new();
let orders = Topic::new("orders").unwrap();
let billing = ConsumerGroup::new("billing").unwrap();
assert!(broker.publish(&orders, Message::new("o-1")).block_on().into_result().is_ok());
let acked = broker
  .subscribe(&orders, &billing, SubscribeOptions::default().prefetch(8))
  .flat_map(|deliveries| deliveries.take(1).run_for_each(|d| d.ack()))
  .block_on();
assert!(acked.into_result().is_ok());
assert_eq!(broker.backlog(&orders, &billing), 0);
```

Tier `integration`.
