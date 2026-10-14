# Async messaging (production)

> **Labs crate — unpublished, no API guarantees ([ADR 0010](https://github.com/Industrial/id_effect/blob/main/docs/adrs/0010-crate-tiers.md)).**
> `id_effect_jobs` and `id_effect_events` live in [`labs/`](https://github.com/Industrial/id_effect/tree/main/labs).
> Use them via a path or git dependency on `labs/`; they are not on crates.io. Build and test them
> with `cargo … --manifest-path labs/Cargo.toml`.

Platform async messaging stacks on a **sqlx `PgPool`** (`id_effect_sql::pg::PgPool`, published,
feature `pg`), with adapters in the labs crates
[`id_effect_jobs`](https://github.com/Industrial/id_effect/tree/main/labs/id_effect_jobs) and
[`id_effect_events`](https://github.com/Industrial/id_effect/tree/main/labs/id_effect_events).

## SQL platform

[`id_effect_sql::pg`](https://docs.rs/id_effect_sql/latest/id_effect_sql/pg/index.html) (feature `pg`; formerly the `id_effect_sql_pg` crate, now a deprecated shim) is built on sqlx:

- `PgSqlClient` implements [`SqlClient`](https://docs.rs/id_effect_sql/latest/id_effect_sql/trait.SqlClient.html)
- `provide_pg_sql_client` registers `PgPool` + `SqlClientService`
- Set `DATABASE_URL` (devenv provides `postgresql://postgres@127.0.0.1:5432/id_effect`)

## Jobs — Apalis (pull workers)

`ApalisJobQueue` is **enqueue-only**. Workers pull tasks via Apalis `WorkerBuilder` + `PostgresStorage::poll`. There is no FIFO `dequeue` on the storage side. `id_effect_jobs` uses the stable apalis 1.0 family.

```rust,ignore
use id_effect_jobs::{ApalisJobQueue, JobSpec};
use id_effect::run_async;

ApalisJobQueue::setup(&pool).await?;
let queue = ApalisJobQueue::new(&pool, "app_jobs");
run_async(queue.enqueue(JobSpec::new("notify", b"payload")), ()).await?;
```

Enable: `id_effect_jobs` features `apalis` (+ `postgres`).

## Transactional outbox — obix

`ObixOutbox` persists `OutboxRecord` rows through [obix](https://docs.rs/obix) on the shared pool. Relay is driven by obix's [`register_event_handler`](https://docs.rs/obix/latest/obix/struct.Outbox.html#method.register_event_handler); the per-consumer cursor lives in `job_executions.execution_state_json`.

For unit tests only: `memory` feature + `MemoryOutbox` + `relay_outbox`.

## Idempotent inbox — obix + job

`ObixInbox` wires obix [`Inbox`](https://docs.rs/obix/latest/obix/struct.Inbox.html) to the [`job`](https://docs.rs/job) poller for idempotent consumers.

## Kafka — rdkafka

`RdKafkaBroker` implements `MessageBroker` with [rdkafka](https://docs.rs/rdkafka). `KafkaBrokerStub` is an in-memory test double (`memory` feature).

## SQL event journal

`EsEntityPgBackend` (`id_effect_events` feature `es-entity`) implements `SqlJournalBackend` on the same `PgPool`.

Apply `ES_ENTITY_EVENT_JOURNAL_DDL` via `apply_es_entity_journal_ddl`.

## Feature matrix

| Crate | Feature | Adapter |
|-------|---------|---------|
| `id_effect_jobs` (labs) | `memory` (default) | in-process test doubles |
| `id_effect_jobs` (labs) | `apalis` | Apalis PostgreSQL queue |
| `id_effect_jobs` (labs) | `obix` | obix outbox + inbox |
| `id_effect_jobs` (labs) | `kafka` | rdkafka broker |
| `id_effect_events` (labs) | `postgres` | `EsEntityPgBackend` |

## Examples

```bash
cargo run --manifest-path labs/Cargo.toml -p id_effect_jobs --example 010_messaging_e2e
cargo run --manifest-path labs/Cargo.toml -p id_effect_jobs --example 020_events_workflow_e2e
```

## See also

- ADR: [`docs/platform/adrs/adr-sql-driver-choice.md`](https://github.com/Industrial/id_effect/blob/main/docs/platform/adrs/adr-sql-driver-choice.md)
- [Data access (`id_effect_sql`)](./ch28-00-data.md)
