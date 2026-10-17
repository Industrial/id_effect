# `id_effect_jobs`

Background jobs for id_effect services: a Postgres `SKIP LOCKED` queue on `id_effect_sql`,
workers with per-queue concurrency, retries with backoff and a `dead` state, unique keys, `run_at`,
and cron schedules that run once per cluster (ADR 0010 Amendment 6).

The crate grows over the 0.10 release. Today it defines `JobState`, the lifecycle every queued job
moves through and the value of the queue table's `state` column:

```rust
use id_effect_jobs::JobState;

let state: JobState = "dead".parse().unwrap();
assert!(state.is_terminal());
assert_eq!(state.to_string(), "dead");
```

Tier `integration`.
