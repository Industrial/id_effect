# Background jobs (`id_effect_jobs`)

`id_effect_jobs` runs the work a service puts off until later: a typed job is enqueued on a PostgreSQL table, a worker claims it with `FOR UPDATE SKIP LOCKED`, runs its handler, and retries a failure with backoff until it succeeds or is `dead`. Unique keys, priorities, a first `run_at`, per-queue concurrency and cron schedules come with it; `MemoryJobQueue` is the same queue in memory, for tests.

```toml
id_effect_jobs = { version = "0.10", features = ["cron"] }  # `cron` only for cron schedules
```

## Jobs

A job is a `#[derive(Schema)]` struct that implements `Job`. The struct is the payload: it is encoded with its schema and stored as `jsonb`, and decoded again for the handler. Only `NAME` is required:

```rust
# use id_effect::Schema;
# use id_effect_jobs::{Job, UniqueKey};
#[derive(Clone, Schema)]
struct SendReceipt {
  order: i64,
}

impl Job for SendReceipt {
  const NAME: &'static str = "shop.send_receipt"; // the registry key: keep it stable across deploys
  const QUEUE: &'static str = "mail";               // default `default`

  fn unique_key(&self) -> Option<UniqueKey> {
    Some(UniqueKey::while_pending(format!("receipt-{}", self.order)))
  }
}
```

| Item | Default | Means |
|---|---|---|
| `NAME` | — | the handler's key and the `name` column; renaming it strands queued jobs of the old name |
| `QUEUE` | `"default"` | the queue it is enqueued on |
| `priority(&self)` | `Priority::NORMAL` (0) | higher runs first within a queue (`LOW` −10, `HIGH` 10, any `i16`) |
| `max_attempts(&self)` | 20 | attempts before the job is `dead` |
| `unique_key(&self)` | none | deduplicates within the queue (below) |
| `timeout(&self)` | none | an attempt that runs longer fails and is retried |
| `retry()` | `default_retry()`: exponential from 1 s, capped at 1 h, ±20 % jitter | the core `Schedule` between attempts |

## Enqueuing

`JobQueueExt` gives every `JobQueue` typed enqueue methods; each returns `Enqueued::New(id)` or, when a job holding the same unique key exists, `Enqueued::Duplicate { existing }` (nothing is inserted):

| Call | Runs |
|---|---|
| `queue.enqueue(job, JobOptions::new())` | now, with the job's own queue, priority and key |
| `queue.enqueue_at(job, at)` | at the instant `at` (a `UtcDateTime`) |
| `queue.enqueue_after(job, delay)` | `delay` after the insert |

`JobOptions` overrides the job's choices per enqueue: `.queue("reports")`, `.priority(Priority::HIGH)`, `.run_at(RunAt::After(d))`, `.unique(key)`. Inside an effect that has the queue in its environment, `id_effect_jobs::enqueue(job, opts)` reads the `JobQueueService` capability with `need`.

### In the caller's transaction

`enqueue_in(&tx, job, opts, &clock)` inserts with the caller's `SqlTransaction`: the job commits or rolls back with the work that caused it, so a receipt is never sent for an order that was rolled back. A unique-key conflict inserts nothing and leaves `tx` usable. It needs only the `id_effect_jobs` table and the queue's clock (`PgJobQueue::clock`); an in-process worker picks the job up at its next poll.

```rust,no_run
use std::sync::Arc;

use id_effect::{Effect, Schema, WallClock};
use id_effect_jobs::{Job, JobError, JobOptions, enqueue_in};
use id_effect_sql::{SqlClient, SqlTransaction, params, with_transaction};

#[derive(Clone, Schema)]
struct SendReceipt {
  order: i64,
}

impl Job for SendReceipt {
  const NAME: &'static str = "shop.send_receipt";
}

fn place_order(client: Arc<dyn SqlClient>, order: i64, clock: WallClock) -> Effect<(), JobError> {
  with_transaction(client, move |tx: Arc<dyn SqlTransaction>| {
    tx.execute("INSERT INTO orders (id) VALUES ($1)", &params![order])
      .map_err(JobError::Sql)
      .flat_map(move |_| enqueue_in(&tx, SendReceipt { order }, JobOptions::new(), &clock))
      .map(|_| ())
  })
}
# fn main() {}
```

## Workers

A `JobRegistry` maps job names to handlers; a handler gets the decoded job and a `JobContext` (`id`, `name`, `queue`, `attempt` from 1, `max_attempts`, `inserted_at`) and returns an effect. Its error type is anything that converts into `JobFailure` (`String`, `&str`, `SqlError`, `Never`, `JobFailure` itself). Registering a name twice is `JobError::DuplicateJob`.

`Worker::new(registry)` runs them. `.queue(name, n)` serves a queue with at most `n` attempts at a time in this worker (without any `.queue` it serves `default` with 10), so a slow queue cannot starve the others. The handlers' environment must hold the `JobQueueService` the worker claims from:

| Run it with | For |
|---|---|
| `worker.run()` | a fiber that polls until interrupted; interrupting it drains |
| `worker.layer()` | a scoped layer: starts the worker when built, provides its `WorkerHandle` (`in_flight()`), drains when the scope closes |
| `worker.run_until_idle()` | tests and batch programs: claims and runs until nothing is runnable, returns a `DrainReport` |

Each queue has one poll fiber. It claims as many jobs as it has free permits (at most 32), runs each in its own fiber inside a span `job.run` (`job.name`, `job.queue`, `job.id`, `job.attempt`), and polls again at once after a full batch, otherwise after `poll_interval` or as soon as a job is enqueued through the same store in this process. There is no `LISTEN/NOTIFY`: polling stays the source of truth.

`WorkerConfig` holds the rest:

| Field | Default | |
|---|---|---|
| `id` | `WorkerId::generate()` | the `locked_by` of its jobs |
| `poll_interval` | 1 s | longest wait between polls of an idle queue |
| `lease` | 5 min | a claimed job's lease; a heartbeat renews it every third |
| `rescue_every` | 30 s | how often it re-queues jobs whose lease expired and prunes |
| `drain_timeout` | 30 s | on shutdown, how long it waits for running attempts before interrupting them |
| `prune_after` | 7 days | finished jobs older than this are deleted (`None` keeps them) |
| `clock` | `WallClock::system()` | the worker's own times |
| `observer` | `NoopObserver` | a `JobObserver` that sees every claim and outcome (metrics) |

A worker that crashes or hangs stops heartbeating; once its lease expires another worker rescues its jobs and they run again. A late outcome of a rescued job is dropped (fenced by the worker id). On shutdown the worker stops claiming, waits up to `drain_timeout`, interrupts the rest, and puts their jobs back to `scheduled` with the attempt refunded.

## Retries and `dead`

An attempt fails when the handler fails, dies, panics, exceeds `timeout`, or names no registered job. The job then goes to `failed` and runs again after the delay of its `retry()` schedule for that attempt — or becomes `dead` when its attempts are exhausted, its schedule is done, or the failure is `JobFailure::discard(..)` (retrying cannot help; a payload that no longer decodes is discarded this way). `JobFailure::retry(..)` is the ordinary case. The text of the last failure (at most 4 KiB) is kept in `last_error`.

A `Schedule` keeps its state in memory only, so the delay is replayed from the attempt number (`retry_delay(&schedule, &failure, attempt)`); clock-free schedules replay exactly.

`dead` and `cancelled` jobs stay in the table for inspection: `queue.retry_now(id)` puts one back to `scheduled` with its attempts reset, and `queue.cancel(id)` cancels a `scheduled` or `failed` job (a running one is not interrupted).

| State | Means | Next |
|---|---|---|
| `scheduled` | waiting for its `run_at` and a free worker | `running`, `cancelled` |
| `running` | claimed, leased to a worker | `succeeded`, `failed`, `dead` |
| `failed` | an attempt failed; runnable again at its new `run_at` | `running`, `cancelled` |
| `succeeded` | done | pruned |
| `dead` | out of attempts or discarded | `retry_now`, pruned |
| `cancelled` | cancelled before it ran | `retry_now`, pruned |

## Unique keys

A `UniqueKey` deduplicates jobs within a queue — the partial unique index on `(queue, unique_key)`:

- `UniqueKey::while_pending(key)` holds the key while the job is `scheduled`, `running` or `failed`; once it succeeds, dies or is cancelled the key is free again. "Send receipt 7" is enqueued once however many times the order is saved.
- `UniqueKey::until_pruned(key)` holds it until the row is pruned, whatever its state: a cron tick runs once even when a second scheduler enqueues it after the first run finished.

An enqueue that conflicts inserts nothing (`ON CONFLICT DO NOTHING`) and reports `Enqueued::Duplicate`; inside a transaction it does not abort it.

## Priorities and `run_at`

Workers claim the runnable jobs of a queue whose `run_at` has come, highest `priority` first, then earliest `run_at`. A job with a later `run_at` is not runnable before it, whatever its priority. Every time the queue reads — `run_at`, leases, `finished_at` — comes from its `WallClock`, never the database's `now()`: anchor the clock to a `TestExecutor` and a test moves an hour ahead in a microsecond. The nodes' clocks must agree to well within the lease.

## Cron

With the feature `cron`, `enqueue_on_tick(name, cron, make)` turns a core `CronJob` into a producer: at every fire time it enqueues `make(&tick)` keyed `tick_key(name, tick)` = `{name}@{tick}` (`until_pruned`), runnable at the tick. The job then runs on any worker with the queue's retries. However many schedulers run the entry, each tick is inserted once.

`CronLeader` runs a `CronRegistry` on one node at a time: it holds an `id_effect_node` `ClusterSingleton` (in production over a `PgAdvisoryLease`), heartbeats the lease while it runs the schedule, and on a failed heartbeat stops, resigns and competes again; a follower that acquires the lease takes the schedule over. A tick both nodes enqueue around a handover is still inserted once.

```rust
use std::sync::Arc;
use std::time::Duration;

use id_effect::{Cron, CronRegistry, Executor, Exit, Schema, TestExecutor, UtcDateTime, WallClock};
use id_effect_jobs::{
  CronLeader, Job, JobFilter, JobQueue, JobQueueEnv, MemoryJobQueue, enqueue_on_tick,
  job_queue_env,
};
use id_effect_node::singleton::{ClusterSingleton, MemoryLease};
use id_effect_process::{NodeName, Pid};

#[derive(Clone, Schema)]
struct Rollup {
  period: String,
}

impl Job for Rollup {
  const NAME: &'static str = "shop.rollup";
}

let cron = Cron::parse("0 * * * *").unwrap(); // hourly
let registry = CronRegistry::<JobQueueEnv>::new()
  .with(enqueue_on_tick("rollup", cron, |_tick| Rollup { period: "hour".into() }))
  .unwrap();

let exec = TestExecutor::new();
let start = UtcDateTime::from_epoch_millis(1_767_225_600_000).unwrap(); // 2026-01-01T00:00Z
let clock = WallClock::anchored(exec.now(), start);
let store = MemoryJobQueue::new(clock.clone());
// In production: `PgAdvisoryLease::connect(url)` instead of `MemoryLease`.
let lease = Arc::new(MemoryLease::new());
let singleton = ClusterSingleton::new("cron", "node-a", lease, Duration::from_secs(3600));
let leader = CronLeader::new(singleton, Pid::from_parts(NodeName::new("a@host"), 1, 1));
let fiber = leader
  .run(&registry, clock)
  .provide_all(job_queue_env(store.clone()))
  .fork_root(Arc::new(exec.clone()));
exec.advance(Duration::from_secs(3 * 3600));
let Exit::Success(jobs) = store.list(JobFilter::default()).block_on() else {
  panic!("list");
};
assert_eq!(jobs.len(), 3);
assert_eq!(jobs[0].unique.as_ref().unwrap().key, "rollup@2026-01-01T03:00:00Z");
fiber.interrupt_now();
```

## The schema

`PgJobQueue::new(client)` keeps the queue in the table `id_effect_jobs` over any `SqlClient` (`PgSqlClient` in production). Create the table at startup with the crate's `migrator()` — an `id_effect_sql` `Migrator` tracked in `_id_effect_jobs_migrations`, which takes the migration lock so replicas starting together apply it once — or with `PgJobQueue::ensure_schema()`, which runs the same statements idempotently. To keep one migration history, copy `migrations/0001_id_effect_jobs.{up,down}.sql` into the application's migrations instead.

`pg_job_queue_layer()` provides a `PgJobQueue` as the `JobQueueService` over the `SqlClientService` in the environment; `memory_job_queue_layer(store)` does the same for a `MemoryJobQueue`.

```rust,no_run
use std::sync::Arc;

use id_effect::hlist;
use id_effect_jobs::{JobQueueEnv, JobQueueService, JobRegistry, PgJobQueue, Worker, migrator};
use id_effect_sql::pg::{PgPoolConfig, PgSqlClient, pg_pool_from_config};
use id_effect_tokio::TokioRuntime;

async fn run_workers(url: String, registry: JobRegistry<JobQueueEnv>) {
  let pool = pg_pool_from_config(PgPoolConfig::from_url(url)).await.expect("connect");
  let client = PgSqlClient::new(pool);
  let rt = TokioRuntime::current().expect("inside tokio");
  let applied = rt.run(migrator().run_on(Arc::new(client.clone()))).await;
  assert!(applied.into_result().is_ok(), "migrate id_effect_jobs");
  let env = hlist![JobQueueService::new(PgJobQueue::new(client))];
  let worker = Worker::new(registry).queue("default", 10).queue("mail", 4);
  // Runs until interrupted, then drains.
  let _ = rt.run(worker.run().provide_all(env)).await;
}
# fn main() {}
```

`queue.stats()` returns per-queue counts by state with the oldest runnable `run_at`, `queue.list(JobFilter { .. })` the jobs newest first. The [console](./ch27-03-console.md) shows both in its jobs panel (`id_effect_console` feature `jobs`).

## Testing with `MemoryJobQueue`

`MemoryJobQueue::new(clock)` keeps the ordering, uniqueness, leases and fencing of `PgJobQueue` in memory. With its clock anchored to a `TestExecutor`, retries and `run_at` play out on virtual time, and `run_until_idle` runs everything that is due:

```rust
use std::sync::{Arc, Mutex};
use std::time::Duration;

use id_effect::{
  Effect, Executor, Exit, Never, Schedule, Schema, TestExecutor, UtcDateTime, WallClock, fail,
  hlist,
};
use id_effect_jobs::{
  DrainReport, Job, JobError, JobFailure, JobOptions, JobQueueExt, JobQueueService, JobRegistry,
  JobQueueEnv, MemoryJobQueue, Worker, WorkerConfig,
};

#[derive(Clone, Schema)]
struct SyncWarehouse {
  sku: String,
}

impl Job for SyncWarehouse {
  const NAME: &'static str = "shop.sync_warehouse";

  fn max_attempts(&self) -> u16 {
    2
  }

  fn retry() -> Schedule<(), JobFailure> {
    Schedule::spaced(Duration::from_secs(60)).map(|_| ())
  }
}

let exec = TestExecutor::new();
let clock = WallClock::anchored(exec.now(), UtcDateTime::unsafe_make(1_800_000_000_000));
let store = MemoryJobQueue::new(clock.clone());
let registry = JobRegistry::<JobQueueEnv>::new()
  .register::<SyncWarehouse, String>(|job, _ctx| fail(format!("{} is not stocked", job.sku)))
  .unwrap();
let worker = Worker::new(registry).with_config(WorkerConfig { clock, ..WorkerConfig::default() });

// Runs `eff` on the test executor until it stalls.
let run = |eff: Effect<DrainReport, JobError>| {
  let slot = Arc::new(Mutex::new(None));
  let out = Arc::clone(&slot);
  let _fiber = eff
    .exit::<Never>()
    .map(move |exit| *out.lock().unwrap() = Some(exit))
    .fork_root(Arc::new(exec.clone()));
  exec.run_until_stalled();
  match slot.lock().unwrap().take() {
    Some(Exit::Success(report)) => report,
    other => panic!("{other:?}"),
  }
};
let env = || hlist![JobQueueService::new(store.clone())];
let sync = SyncWarehouse { sku: "SKU-1".into() };
let first = store.enqueue(sync, JobOptions::new()).flat_map({
  let (worker, env) = (worker.clone(), env());
  move |_| worker.run_until_idle().provide_all(env)
});
assert_eq!(run(first).retrying, 1);
exec.advance(Duration::from_secs(60)); // the backoff passes in no time
assert_eq!(run(worker.run_until_idle().provide_all(env())).dead, 1);
```

`MemoryJobQueue::fail_with(Some("down"))` makes every store call fail until `fail_with(None)`, to test what a service does when the queue is unavailable. The example `cargo run -p id_effect_jobs --example 010_jobs` walks through unique keys, priorities, `run_at`, retries to `dead` and a manual retry on the memory queue.

Run the queue's own PostgreSQL tests with a database at `DATABASE_URL`; `ID_EFFECT_REQUIRE_PG=1` turns a missing database into a failure instead of a skip:

```bash
ID_EFFECT_REQUIRE_PG=1 DATABASE_URL=postgres://postgres@127.0.0.1:5432/id_effect \
  cargo nextest run -p id_effect_jobs --all-features
```
