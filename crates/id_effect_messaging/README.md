# `id_effect_messaging`

Messaging for id_effect services: the broker-agnostic `MessageBroker` (publish, subscribe, ack), an
in-memory broker, and the transactional outbox, inbox and relay on `id_effect_sql`. Redis Streams
implement the broker in `id_effect_redis` (feature `streams`) (ADR 0010 Amendment 6).

The crate grows over the 0.10 release. Today it defines `Topic`, the validated name messages are
published to: 1 to 249 ASCII letters, digits, `.`, `_` or `-`, a name every supported broker
accepts.

```rust
use id_effect_messaging::{Topic, TopicError};

let topic = Topic::new("ledger.entry-posted").unwrap();
assert_eq!(topic.as_str(), "ledger.entry-posted");
assert_eq!(Topic::new(""), Err(TopicError::Empty));
```

Tier `integration`.
