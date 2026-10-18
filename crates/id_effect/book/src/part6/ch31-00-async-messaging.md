# Async messaging and jobs

Work that should not happen inside a request — sending a receipt, rebuilding a report, telling another service that an order was placed — leaves the request through one of three published crates. All three are tier `integration` ([ADR 0010](https://github.com/Industrial/id_effect/blob/main/docs/adrs/0010-crate-tiers.md), Amendment 6) and run on PostgreSQL through [`id_effect_sql`](./ch28-00-data.md), with in-memory twins for tests.

| Crate | For | Chapter |
|---|---|---|
| [`id_effect_jobs`](https://docs.rs/id_effect_jobs) | background work of *this* service: a typed job, run later by a worker, retried until it succeeds or is `dead`; cron schedules | [Background jobs](./ch31-01-jobs.md) |
| [`id_effect_messaging`](https://docs.rs/id_effect_messaging) | facts *other* consumers react to: publish to a topic, consumer groups, dead-letter topics; the transactional outbox, relay and idempotent inbox | [Messaging](./ch31-02-messaging.md) |
| [`id_effect_mail`](https://docs.rs/id_effect_mail) | outgoing mail: SMTP or SendGrid, templates, a preview in the browser | [Mail](./ch31-03-mail.md) |

```toml
id_effect_jobs = { version = "0.10", features = ["cron"] }  # `cron` only for cron schedules
id_effect_messaging = "0.10"
```

## Jobs or messages

A **job** names the code that runs it: `SendReceipt { order: 7 }` is run by the one handler registered for `shop.send_receipt`, on a queue this service owns. A **message** names what happened: `order.placed` is published once and every consumer group subscribed to the topic gets it — billing, search, the warehouse — each in its own code, possibly in other services. Use a job when you know who does the work, a message when you do not want to.

Both are delivered **at least once**: a worker or consumer that dies mid-run sees the work again, so handlers must be idempotent. Jobs deduplicate on a unique key; consumers deduplicate on the message id with the inbox.

## Committing with the caller's work

The usual bug is a job or message that leaves when the transaction that caused it rolls back (or the other way round). Both crates insert with the caller's `SqlTransaction`:

- `id_effect_jobs::enqueue_in(&tx, job, opts, &clock)` inserts the job into `id_effect_jobs` with `tx`.
- `id_effect_messaging::Outbox::stage(&tx, &topic, message)` writes the message to `id_effect_outbox` with `tx`; a `Relay` publishes it after the commit.

Either way the row exists if and only if the caller's work committed.

## Brokers

`id_effect_messaging` defines the broker-agnostic `MessageBroker` and ships `MemoryBroker`, which keeps every semantic of the trait (consumer groups, redelivery, dead letters, prefetch) in one process. The Redis Streams broker lives in `id_effect_redis` behind the feature `streams`. A relay publishes through any of them.

## Schemas

Each crate ships its tables as an `id_effect_sql` `Migrator` with its own tracking table, so its version numbers never collide with the application's migrations:

| Crate | Tables | Migrator | Tracked in |
|---|---|---|---|
| `id_effect_jobs` | `id_effect_jobs` | `id_effect_jobs::migrator()` | `_id_effect_jobs_migrations` |
| `id_effect_messaging` | `id_effect_outbox`, `id_effect_inbox` | `id_effect_messaging::migrator()` | `_id_effect_messaging_migrations` |

Run them at startup (`migrator().run_on(client)`), or copy their `migrations/*.sql` into the application's own.

## Examples

```bash
cargo run -p id_effect_jobs --example 010_jobs               # queue, worker, retries: no database
cargo run -p id_effect_messaging --example 020_memory_broker
cargo run -p id_effect_messaging --example 030_outbox_inbox
```

## See also

- [Events and projections](../part5/ch23-00-events-and-projections.md) — event sourcing with `id_effect_events`
- [Console](./ch27-03-console.md) — the jobs panel (`id_effect_console` feature `jobs`)
- ADR: [`docs/platform/adrs/adr-sql-driver-choice.md`](https://github.com/Industrial/id_effect/blob/main/docs/platform/adrs/adr-sql-driver-choice.md)
