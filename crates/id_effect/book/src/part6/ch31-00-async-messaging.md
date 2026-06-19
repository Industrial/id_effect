# Background jobs and reliable messaging

HTTP handlers should stay fast. Work that can fail, retry, or run later—sending email, charging a card, fan-out notifications—belongs on a **background path** with clear delivery semantics.

This chapter introduces `id_effect_jobs` for in-process job queues and transactional outbox relay, and ties them to `id_effect_events` when you need a durable audit trail.

## What you'll learn

- How to enqueue work with `MemoryJobRunner` and drain it in tests or a worker loop.
- The transactional outbox pattern and the crate's `MemoryOutbox` + `relay_outbox` MVP.
- When the in-memory `KafkaBrokerStub` is appropriate versus a real broker.
- How `SqlEventJournal` persists domain events for projections and replay.

## Prerequisites

- [Application host](./ch30-00-application.md) — where HTTP and process lifecycle live.
- [Error handling](../part3/ch08-00-error-handling.md) — modeling failures in effectful job handlers.

## Why not `spawn` from every handler?

Fire-and-forget `tokio::spawn` loses **backpressure**, **retry policy**, and **at-least-once** guarantees. A job runner lets you:

- Run the same handler in tests synchronously (`drain_jobs`).
- Attach logging and metrics at one boundary.
- Evolve toward outbox + broker without rewriting domain code.

## In-process jobs

`MemoryJobRunner` stores pending jobs; `drain_jobs` executes them on the current thread—ideal for unit tests and single-node prototypes:

```rust,no_run
use id_effect_jobs::{MemoryJobRunner, drain_jobs};

let runner = MemoryJobRunner::new();
// enqueue from an Effect step or handler...
drain_jobs(&runner)?;
```

Model each job as an `Effect` that receives capabilities in `R` (database, HTTP client) so production uses the same code path with a different runner implementation later.

## Transactional outbox

When a database commit and a message publish must succeed together, write the message to an **outbox table** in the same transaction, then relay asynchronously:

```rust,no_run
use id_effect_jobs::{MemoryOutbox, relay_outbox};

let outbox = MemoryOutbox::new();
// application writes row + outbox entry atomically in your DB layer
relay_outbox(&outbox, |topic, payload| {
    // publish to broker
    Ok(())
})?;
```

`MemoryOutbox` documents the API; production swaps in SQL-backed storage and a real broker client.

## Kafka adapter stub

`KafkaBrokerStub` implements `MessageBroker` with in-memory fan-out until you add `rdkafka`:

```rust
use id_effect::run_blocking;
use id_effect_jobs::{KafkaBrokerStub, MessageBroker};

let broker = KafkaBrokerStub::new("localhost:9092");
run_blocking(broker.publish("orders.created", br"{}", ()), ())?;
```

Use it to teach topic naming and payload contracts in CI without a cluster. Replace the stub when you need partitioning, consumer groups, or cross-service delivery guarantees.

## SQL event journal

`SqlEventJournal` persists append-only domain events. Apply `POSTGRES_JOURNAL_DDL` once, then append from effectful command handlers. Tests use `TestSqlJournalBackend` without PostgreSQL when you only need persistence semantics.

Pair journals with [Events and projections](../part5/ch23-00-events-and-projections.md) when building read models.

## Putting it together

A minimal order flow:

1. HTTP handler commits order + outbox row in one transaction.
2. Background task calls `relay_outbox` to publish `orders.created`.
3. A consumer job updates a read model or calls an external API, with retries via [Scheduling](../part3/ch11-00-scheduling.md).

Start with `MemoryJobRunner` and `MemoryOutbox` in tests; promote to SQL + real broker when load and durability require it.

## Summary

Treat **jobs** as effect programs with a runner boundary, and **messaging** as either in-process stubs or outbox-backed delivery—not ad hoc spawns from handlers. The crates here are MVPs that encode the pattern; infrastructure choices stay yours.

## Next steps

- [Workflow and cluster](./ch32-00-workflow-cluster.md) — durable multi-step processes when jobs are not enough.
