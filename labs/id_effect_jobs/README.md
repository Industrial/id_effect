# id_effect_jobs

Production async messaging for [id_effect](https://github.com/Industrial/id_effect).

## Features

| Feature | Description |
|---------|-------------|
| `memory` (default) | In-process `MemoryJobRunner`, `MemoryOutbox`, `MemoryBroker`, `KafkaBrokerStub` |
| `apalis` | `ApalisJobQueue` — enqueue-only; workers pull via Apalis (takes `apalis_postgres::PgPool`, sqlx 0.9) |
| `obix` | `ObixOutbox`, `ObixInbox` on shared `sqlx::PgPool` |
| `kafka` | `RdKafkaBroker` via rdkafka |

## Quick start

```toml
id_effect_jobs = { path = "../id_effect_jobs", features = ["apalis", "obix"] }
```

Every operation returns an `id_effect::next::Effect` (0.7). Set `DATABASE_URL` (devenv:
`postgresql://postgres@127.0.0.1:5432/id_effect`) to run, and build the pg features with
`SQLX_OFFLINE=true` (apalis-postgres checks its queries against the database otherwise).

See `examples/messaging_e2e.rs` and book chapter `part6/ch31-00-async-messaging.md`.
