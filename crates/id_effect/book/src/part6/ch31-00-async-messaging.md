# Async messaging (production)

Platform async messaging stacks on **sqlx `PgPool`** ([`PgPool`](../../../id_effect_sql_pg)), with production adapters in [`id_effect_jobs`](../../../id_effect_jobs) and [`id_effect_events`](../../../id_effect_events).

## SQL platform

[`id_effect_sql_pg`](../../../id_effect_sql_pg) replaces the deadpool era with sqlx:

- [`PgSqlClient`](../../../id_effect_sql_pg/struct.PgSqlClient.html) implements [`SqlClient`](../../../id_effect_sql/trait.SqlClient.html)
- [`provide_pg_sql_client`](../../../id_effect_sql_pg/fn.provide_pg_sql_client.html) registers `PgPool` + `SqlClientService`
- Set `DATABASE_URL` (devenv provides `postgresql://postgres@127.0.0.1:5432/id_effect`)

## Jobs — Apalis (pull workers)

[`ApalisJobQueue`](../../../id_effect_jobs/struct.ApalisJobQueue.html) is **enqueue-only**. Workers pull tasks via Apalis `WorkerBuilder` + `PostgresStorage::poll` — there is no FIFO `dequeue` on the storage side.

```rust,ignore
use id_effect_jobs::{ApalisJobQueue, JobSpec};
use id_effect::run_async;

ApalisJobQueue::setup(&pool).await?;
let queue = ApalisJobQueue::new(&pool, "app_jobs");
run_async(queue.enqueue(JobSpec::new("notify", b"payload")), ()).await?;
```

Enable: `id_effect_jobs` features `apalis` (+ `postgres`).

## Transactional outbox — obix

[`ObixOutbox`](../../../id_effect_jobs/struct.ObixOutbox.html) persists [`OutboxRecord`](../../../id_effect_jobs/struct.OutboxRecord.html) rows through [obix](https://docs.rs/obix) on the shared pool. Relay is driven by obix's [`register_event_handler`](https://docs.rs/obix/latest/obix/struct.Outbox.html#method.register_event_handler); the per-consumer cursor lives in `job_executions.execution_state_json`.

For unit tests only: `memory` feature + [`MemoryOutbox`](../../../id_effect_jobs/struct.MemoryOutbox.html) + [`relay_outbox`](../../../id_effect_jobs/fn.relay_outbox.html).

## Idempotent inbox — obix + job

[`ObixInbox`](../../../id_effect_jobs/struct.ObixInbox.html) wires obix [`Inbox`](https://docs.rs/obix/latest/obix/struct.Inbox.html) to the [`job`](https://docs.rs/job) poller for idempotent consumers.

## Kafka — rdkafka

[`RdKafkaBroker`](../../../id_effect_jobs/struct.RdKafkaBroker.html) implements [`MessageBroker`](../../../id_effect_jobs/trait.MessageBroker.html) with [rdkafka](https://docs.rs/rdkafka). [`KafkaBrokerStub`](../../../id_effect_jobs/struct.KafkaBrokerStub.html) remains **memory-only** (`memory` feature) for tests.

## SQL event journal

[`EsEntityPgBackend`](../../../id_effect_events/struct.EsEntityPgBackend.html) (`id_effect_events` feature `es-entity`) implements [`SqlJournalBackend`](../../../id_effect_events/trait.SqlJournalBackend.html) on the same `PgPool`.

Apply [`ES_ENTITY_EVENT_JOURNAL_DDL`](../../../id_effect_events/constant.ES_ENTITY_EVENT_JOURNAL_DDL.html) via [`apply_es_entity_journal_ddl`](../../../id_effect_events/fn.apply_es_entity_journal_ddl.html).

## Feature matrix

| Crate | Feature | Adapter |
|-------|---------|---------|
| `id_effect_jobs` | `memory` (default) | in-process stubs |
| `id_effect_jobs` | `apalis` | Apalis PostgreSQL queue |
| `id_effect_jobs` | `obix` | obix outbox + inbox |
| `id_effect_jobs` | `kafka` | rdkafka broker |
| `id_effect_events` | `postgres` | `EsEntityPgBackend` |

## See also

- ADR: `docs/platform/adrs/adr-sql-driver-choice.md`
- Plan: `.cursor/plans/platform_messaging_production.plan.md`
