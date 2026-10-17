# `id_effect_jobs`

Background jobs for id_effect services: a Postgres `SKIP LOCKED` queue on `id_effect_sql`,
workers with per-queue concurrency, retries with backoff and a `dead` state, unique keys, `run_at`,
and cron schedules that run once per cluster (ADR 0010 Amendment 6).

- **Jobs** are `#[derive(Schema)]` structs implementing `Job` (a stable `NAME`, queue, priority,
  `max_attempts`, optional `UniqueKey`, retry `Schedule`); the payload is stored as `jsonb`.
- **`JobQueue`** is the capability: `enqueue` / `enqueue_at` / `enqueue_after`, `cancel`,
  `retry_now`, `stats`, `list`. `PgJobQueue` stores jobs in the table `id_effect_jobs` (schema:
  `migrator()`, tracked in `_id_effect_jobs_migrations`, or `PgJobQueue::ensure_schema`);
  `MemoryJobQueue` keeps them in memory for tests.
- **`enqueue_in(&tx, job, opts, &clock)`** inserts with the caller's transaction, so the job
  commits or rolls back with the caller's work.
- **`Worker`** runs a `JobRegistry`'s handlers: per-queue concurrency, a lease renewed by a
  heartbeat, rescue of crashed workers' jobs, retries until `dead`, and a drain on shutdown.

States: `scheduled → running → succeeded`, `failed` (awaiting a retry) `→ running …`, `dead`, and
`cancelled`. Every time the queue reads comes from a `WallClock`, so tests run on virtual time.
Delivery is at least once: handlers must be idempotent.

```rust
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use id_effect::{Executor, Schema, TestExecutor, UtcDateTime, WallClock, hlist, sync};
use id_effect_jobs::{
  Job, JobFailure, JobOptions, JobQueueExt, JobQueueService, JobRegistry, JobState,
  MemoryJobQueue, Worker,
};

#[derive(Clone, Schema)]
struct SendInvoice {
  invoice: i64,
}

impl Job for SendInvoice {
  const NAME: &'static str = "billing.send_invoice";
  const QUEUE: &'static str = "mail";
}

let sent = Arc::new(AtomicU32::new(0));
let s = Arc::clone(&sent);
let registry = JobRegistry::new()
  .register::<SendInvoice, JobFailure>(move |job, _ctx| {
    let s = Arc::clone(&s);
    sync(move || {
      assert_eq!(job.invoice, 7);
      s.fetch_add(1, Ordering::SeqCst);
    })
  })
  .unwrap();

let exec = TestExecutor::new();
let clock = WallClock::anchored(exec.now(), UtcDateTime::unsafe_make(1_800_000_000_000));
let store = MemoryJobQueue::new(clock);
let env = hlist![JobQueueService::new(store.clone())];
let worker = Worker::new(registry).queue("mail", 4);
let program = store
  .enqueue(SendInvoice { invoice: 7 }, JobOptions::default())
  .flat_map(move |_| worker.run_until_idle().provide_all(env));
let done = program.fork_root(Arc::new(exec.clone()));
exec.run_until_stalled();
assert!(done.is_done());
assert_eq!(sent.load(Ordering::SeqCst), 1);

let state: JobState = "dead".parse().unwrap();
assert!(state.is_terminal());
```

Tier `platform`: the Ledger reference app under `examples/reference/` depends on it.
