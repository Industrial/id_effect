# Async messaging

[`id_effect_jobs`](../../../id_effect_jobs) and [`id_effect_events`](../../../id_effect_events) cover background work and durable event journals.

## Job runner and outbox

- [`MemoryJobRunner`](https://docs.rs/id_effect_jobs/latest/id_effect_jobs/struct.MemoryJobRunner.html) + [`drain_jobs`](https://docs.rs/id_effect_jobs/latest/id_effect_jobs/fn.drain_jobs.html)
- [`MemoryOutbox`](https://docs.rs/id_effect_jobs/latest/id_effect_jobs/struct.MemoryOutbox.html) + [`relay_outbox`](https://docs.rs/id_effect_jobs/latest/id_effect_jobs/fn.relay_outbox.html) — transactional outbox MVP

## Kafka adapter stub

[`KafkaBrokerStub`](https://docs.rs/id_effect_jobs/latest/id_effect_jobs/struct.KafkaBrokerStub.html) implements [`MessageBroker`](https://docs.rs/id_effect_jobs/latest/id_effect_jobs/trait.MessageBroker.html) with in-memory fan-out until `rdkafka` lands:

```rust
use id_effect_jobs::{KafkaBrokerStub, MessageBroker};
use id_effect::run_blocking;

let broker = KafkaBrokerStub::new("localhost:9092");
run_blocking(broker.publish("orders.created", br"{}", ()), ())?;
```

## SQL event journal

[`SqlEventJournal`](https://docs.rs/id_effect_events/latest/id_effect_events/struct.SqlEventJournal.html) hardens persistence with [`POSTGRES_JOURNAL_DDL`](https://docs.rs/id_effect_events/latest/id_effect_events/constant.POSTGRES_JOURNAL_DDL.html) and [`TestSqlJournalBackend`](https://docs.rs/id_effect_events/latest/id_effect_events/struct.TestSqlJournalBackend.html) for unit tests.

## See also

- Mission: `platform-async-messaging`
