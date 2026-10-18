---
title: 'E7.1 queue-core design note (r10-e7-1-queue-core, tsk-mvcjv3wa-40y1j9)'
slug: r10-e7-1-queue-design
status: approved by the owner 2026-10-17 (Q1 keep `scheduled` + add `cancelled`; Q2-Q5 as recommended)
binding: .maestro/specs/releases/0.10.0.md §E7.1 + D2 + D5; scorecard msg.jobs (target 88 = 7/8, all but #7)
---

# id_effect_jobs queue core: API, schema, worker

Grounding: skeleton `crates/id_effect_jobs/src/lib.rs` (`JobState`), `id_effect_sql` (`SqlClient`,
`SqlTransaction` = `Arc<dyn SqlTransaction>`, `SqlValue::{Json,Timestamp}`, `params!`, `migrate::Migrator`),
`id_effect_auth/src/session/pg.rs` + `migrations/0001_id_effect_sessions.up.sql` (store, layer and
`ensure_schema` pattern), core `Schedule` (`scheduling/schedule.rs`: `start`/`step`, opaque in-memory state),
`HasSchema<I = Unknown>` (derive output) + `schema::serde_bridge`, `WallClock` (`scheduling/cron.rs`,
`anchored` for virtual time), `CronRegistry` (`cron_job.rs:328`), `ClusterSingleton`/`PgAdvisoryLease`
(`id_effect_node/src/singleton.rs`). Lessons from `labs/id_effect_jobs/src/runner.rs`: untyped `Vec<u8>`
payload, no attempt/lock bookkeeping, no fencing — all fixed below.

## 1. `Job` trait and handler registry (`job.rs`)

```rust
pub trait Job: HasSchema<A = Self, I = Unknown, E = ()> + Send + Sync + 'static {
  const NAME: &'static str;                  // registry key + `name` column; stable across deploys
  const QUEUE: &'static str = "default";
  fn priority(&self) -> Priority { Priority::NORMAL }          // higher runs first
  fn max_attempts(&self) -> u16 { 20 }                         // >= 1
  fn unique_key(&self) -> Option<UniqueKey> { None }           // dedup within QUEUE
  fn timeout(&self) -> Option<Duration> { None }               // per-attempt; None = until done
  fn retry() -> Schedule<(), JobFailure> {                     // backoff between attempts
    Schedule::exponential(Duration::from_secs(1))
      .union(Schedule::spaced(Duration::from_secs(3600)))      // union = shorter delay: cap 1 h
      .jittered().map(|_| ())
  }
}
pub struct Priority(pub i16);            // NORMAL = 0, HIGH = 10, LOW = -10
pub struct UniqueKey { pub key: String, pub scope: UniqueScope }
pub enum UniqueScope { WhilePending, UntilPruned }   // UntilPruned: cron ticks (§6)
#[non_exhaustive] pub enum JobFailure { Retry(String), Discard(String) }  // Discard => dead now
impl JobFailure { pub fn retry(e: impl Display) -> Self; pub fn discard(e: impl Display) -> Self }
impl From<String> / From<&str> / From<SqlError> for JobFailure      // => Retry
pub struct JobContext { pub id: JobId, pub queue: Queue, pub attempt: u16, pub max_attempts: u16,
                        pub inserted_at: UtcDateTime }
pub struct JobId(pub i64);  pub struct Queue(Arc<str>);   // Queue::DEFAULT = "default"
```

`run_at` is per enqueue, not per type (`JobOptions`, §2). Payload encoding: `J::schema().encode(job)` →
`Unknown` → `serde_bridge::serde_json_from_unknown` → `jsonb`; decoding uses `decode_unknown_all`, so a
payload that no longer fits the type fails with every field error (Discard: retrying cannot fix it).

Registry: typed at registration, erased by name, generic over the handlers' environment `R` (same model
as `CronRegistry<R>`):

```rust
pub struct JobRegistry<R = HNil> { /* BTreeMap<&'static str, Arc<dyn Fn(Unknown, JobContext)
                                     -> Effect<(), JobFailure, R> + Send + Sync>> */ }
impl<R: Send + Sync + 'static> JobRegistry<R> {
  pub fn new() -> Self;
  pub fn register<J: Job, E: Into<JobFailure> + Send + 'static>(
    self, handler: impl Fn(J, JobContext) -> Effect<(), E, R> + Send + Sync + 'static,
  ) -> Result<Self, JobError>;                     // JobError::DuplicateJob(name)
  pub fn names(&self) -> impl Iterator<Item = &'static str>;
}
```
`R` is provided the normal way: `Worker::run` is an `Effect<(), Never, R>` (with `R: Selector<
JobQueueService, I>`), so the app runs it under the same layers as its HTTP handlers (`provide_layer`).

## 2. Capability: `JobQueue` (producer + admin) and `JobStore` (worker side) (`queue.rs`)

Object-safe traits over erased jobs; typed sugar in an extension trait and env free functions (pattern of
`id_effect_sql::query`).

```rust
pub struct NewJob { pub name: String, pub queue: Queue, pub payload: serde_json::Value,
  pub priority: Priority, pub max_attempts: u16, pub unique: Option<UniqueKey>, pub run_at: RunAt }
pub enum RunAt { Now, At(UtcDateTime), After(Duration) }      // resolved on the store's WallClock
#[derive(Default)] pub struct JobOptions { /* queue, priority, run_at, unique overrides */ }
pub enum Enqueued { New(JobId), Duplicate { existing: Option<JobId> } }

pub trait JobQueue: Send + Sync + 'static {
  fn enqueue_raw(&self, job: NewJob) -> Effect<Enqueued, JobError>;
  fn cancel(&self, id: JobId) -> Effect<bool, JobError>;      // scheduled|failed -> cancelled; running: false
  fn retry_now(&self, id: JobId) -> Effect<bool, JobError>;   // dead|cancelled -> scheduled, attempts 0 (#7 later)
  fn stats(&self) -> Effect<Vec<QueueStats>, JobError>;       // per queue: count per JobState, oldest run_at
  fn list(&self, filter: JobFilter) -> Effect<Vec<JobRecord>, JobError>; // queue?, state?, limit, before: JobId
}
pub trait JobStore: JobQueue {
  fn claim(&self, queue: &Queue, worker: &WorkerId, limit: u16, lease: Duration) -> Effect<Vec<Claimed>, JobError>;
  fn complete(&self, id: JobId, worker: &WorkerId) -> Effect<Fenced, JobError>;
  fn fail(&self, id: JobId, worker: &WorkerId, f: &JobFailure, retry_in: Option<Duration>) -> Effect<Fenced, JobError>;
  fn heartbeat(&self, worker: &WorkerId, lease: Duration) -> Effect<u64, JobError>;
  fn release(&self, worker: &WorkerId) -> Effect<u64, JobError>;   // drain: back to scheduled, attempt refunded
  fn rescue_expired(&self) -> Effect<u64, JobError>;               // §4, crashed workers
  fn prune(&self, finished_before: UtcDateTime) -> Effect<u64, JobError>;
}
pub enum Fenced { Applied, Lost }   // Lost: lease expired and the row was rescued; outcome dropped + logged
pub trait JobQueueExt: JobQueue {   // blanket impl
  fn enqueue<J: Job>(&self, job: J, opts: JobOptions) -> Effect<Enqueued, JobError>;
  fn enqueue_at<J: Job>(&self, job: J, at: UtcDateTime) -> Effect<Enqueued, JobError>;
  fn enqueue_after<J: Job>(&self, job: J, delay: Duration) -> Effect<Enqueued, JobError>;
}
pub struct JobQueueService(pub Arc<dyn JobStore>);  pub type JobQueueEnv = HCons<JobQueueService, HNil>;
pub fn enqueue<J: Job, R: Selector<JobQueueService, I>, I>(job: J, opts: JobOptions) -> Effect<Enqueued, JobError, R>;
/// #8: the row is inserted by `tx`, so it commits or rolls back with the caller's work. Needs no queue
/// capability (only the Pg schema), so `deliver_later_in(&tx, mail)` takes just the transaction.
pub fn enqueue_in<J: Job>(tx: &Arc<dyn SqlTransaction>, job: J, opts: JobOptions, clock: &WallClock)
  -> Effect<Enqueued, JobError>;
```
`PgJobQueue::new(client: impl SqlClient)`, `::from_service(SqlClientService)`, `.with_clock(WallClock)`
(default `WallClock::system()`), `.ensure_schema()`; `pg_job_queue_layer<E>() -> Layer<JobQueueEnv, E,
SqlClientEnv>` (mirror of `pg_session_store_layer`). `MemoryJobQueue::new(clock: WallClock)` +
`memory_job_queue_layer`; one `Mutex<State>` with a `BTreeMap<JobId, JobRecord>`, a unique map with the
same predicate as the index, ordering identical to the SQL, times read from its `WallClock` (anchored to
`TestExecutor` in tests). `enqueue_in` against a memory backend is out of scope (no commit hook); its unit
test runs on `TestSqlTransaction` and asserts the recorded INSERT, the live one on Pg.

`JobError` (`#[non_exhaustive]`): `Sql(SqlError)`, `Encode(String)`, `DuplicateJob(String)`,
`InvalidOptions(String)` (max_attempts 0, empty queue name), `NotFound(JobId)`.

## 3. Schema (`migrations/0001_id_effect_jobs.up.sql` + `.down.sql`)

```sql
CREATE TABLE IF NOT EXISTS id_effect_jobs (
  id           bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  name         text        NOT NULL,
  queue        text        NOT NULL DEFAULT 'default',
  payload      jsonb       NOT NULL,
  state        text        NOT NULL DEFAULT 'scheduled' CHECK (state IN
                 ('scheduled','running','succeeded','failed','dead','cancelled')),
  priority     smallint    NOT NULL DEFAULT 0,
  attempts     integer     NOT NULL DEFAULT 0 CHECK (attempts >= 0),
  max_attempts integer     NOT NULL DEFAULT 20 CHECK (max_attempts >= 1),
  unique_key   text,
  unique_scope text        NOT NULL DEFAULT 'pending' CHECK (unique_scope IN ('pending','pruned')),
  run_at       timestamptz NOT NULL,
  inserted_at  timestamptz NOT NULL,
  attempted_at timestamptz,
  finished_at  timestamptz,
  locked_by    text,
  locked_until timestamptz,                 -- lease; heartbeat pushes it forward
  last_error   text
);
-- Dequeue: one queue, runnable rows, priority then run_at (#4, #5).
CREATE INDEX IF NOT EXISTS id_effect_jobs_dequeue_idx
  ON id_effect_jobs (queue, priority DESC, run_at, id) WHERE state IN ('scheduled','failed');
-- Uniqueness (#3): while pending, or until pruned for scope 'pruned' (cron name@tick).
CREATE UNIQUE INDEX IF NOT EXISTS id_effect_jobs_unique_idx ON id_effect_jobs (queue, unique_key)
  WHERE unique_key IS NOT NULL
    AND (state IN ('scheduled','running','failed') OR unique_scope = 'pruned');
-- Rescue of expired leases (#1) and pruning / console listing.
CREATE INDEX IF NOT EXISTS id_effect_jobs_lease_idx ON id_effect_jobs (locked_until) WHERE state = 'running';
CREATE INDEX IF NOT EXISTS id_effect_jobs_finished_idx ON id_effect_jobs (finished_at)
  WHERE state IN ('succeeded','dead','cancelled');
```
`failed` = "failed attempt awaiting retry" (skeleton doc), so it is runnable and counts as pending for
uniqueness (Q2). Shipping (Q3): `id_effect_jobs::migrator() -> Migrator` = `Migrator::from_files` over the
embedded scripts `.with_table("_id_effect_jobs_migrations")`, so the crate's version numbers never collide
with the app's; `PgJobQueue::ensure_schema()` runs the same idempotent statements (auth pattern). A future
schema change is `0002_…` in the same table; the files stay copyable into an app's own directory.

## 4. SQL and state machine (`pg.rs`; `$now` is always bound from the store's `WallClock`, Q4)

Enqueue (no SQLSTATE needed; a conflict never aborts the caller's transaction — critical for #8):
```sql
WITH ins AS (INSERT INTO id_effect_jobs (name,queue,payload,priority,max_attempts,unique_key,unique_scope,run_at,inserted_at)
  VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)
  ON CONFLICT (queue, unique_key) WHERE unique_key IS NOT NULL
    AND (state IN ('scheduled','running','failed') OR unique_scope = 'pruned') DO NOTHING RETURNING id)
SELECT id, true AS inserted FROM ins
UNION ALL SELECT id, false FROM id_effect_jobs WHERE queue = $2 AND unique_key = $6
  AND (state IN ('scheduled','running','failed') OR unique_scope = 'pruned') AND NOT EXISTS (SELECT 1 FROM ins)
LIMIT 1                                  -- no row: Duplicate { existing: None } (concurrent committer)
```
Claim (attempt counted at claim, so a crash mid-run still consumes an attempt):
```sql
WITH next AS (SELECT id FROM id_effect_jobs WHERE queue = $1 AND state IN ('scheduled','failed')
  AND run_at <= $2 ORDER BY priority DESC, run_at, id LIMIT $3 FOR UPDATE SKIP LOCKED)
UPDATE id_effect_jobs j SET state='running', attempts=j.attempts+1, attempted_at=$2,
  locked_by=$4, locked_until=$5 FROM next WHERE j.id = next.id
RETURNING j.id, j.name, j.payload, j.attempts, j.max_attempts, j.inserted_at
```
Fenced outcomes (`WHERE id=$1 AND locked_by=$2 AND state='running'`; 0 rows => `Fenced::Lost`):
- success: `state='succeeded', finished_at=$now, locked_by=NULL, locked_until=NULL`.
- failure: if `Discard`, or `attempts >= max_attempts`, or the schedule is done → `state='dead',
  finished_at=$now`; else `state='failed', run_at=$now + delay`. Both set `last_error` (truncated 4 KiB).
- delay: `Schedule` state is opaque and in-memory, so the worker *replays* `J::retry()` from `start()`
  for `attempts` steps with the failure as input and a synthetic clock (start + sum of earlier delays);
  the last step's delay is used. Deterministic for clock-free schedules; jitter re-seeds per replay.
- heartbeat (every `lease/3`): `UPDATE … SET locked_until=$now+lease WHERE locked_by=$1 AND state='running'`.
- rescue (any worker, every `rescue_every`): `UPDATE … SET state = CASE WHEN attempts >= max_attempts
  THEN 'dead' ELSE 'failed' END, run_at=$now, locked_by=NULL, locked_until=NULL, last_error='lease
  expired (worker lost)', finished_at = CASE … END WHERE state='running' AND locked_until < $now` (#1).
- drain release: `SET state='scheduled', attempts=attempts-1, locked_*=NULL WHERE locked_by=$1 AND state='running'`.

Transitions: scheduled→running→{succeeded | failed→running… | dead}; scheduled|failed→cancelled;
dead|cancelled→scheduled (`retry_now`). Delivery is at-least-once; handlers must be idempotent (doc).

## 5. `Worker` (`worker.rs`)

```rust
pub struct WorkerConfig { pub id: WorkerId /* host:pid:rand */, pub poll_interval: Duration /* 1 s */,
  pub lease: Duration /* 5 min */, pub rescue_every: Duration /* 30 s */, pub drain_timeout: Duration /* 30 s */,
  pub prune_after: Option<Duration> /* 7 d */, pub clock: WallClock, pub observer: Arc<dyn JobObserver> }
pub struct Worker<R> { registry: JobRegistry<R>, queues: Vec<(Queue, NonZeroU16)>, config: WorkerConfig }
impl<R: Send + Sync + 'static> Worker<R> {
  pub fn new(registry: JobRegistry<R>) -> Self;
  pub fn queue(self, name: &str, concurrency: u16) -> Self;   // #6, per worker process
  pub fn run<I>(self) -> Effect<(), Never, R> where R: Selector<JobQueueService, I>;  // until interrupted
  pub fn layer<I>(self) -> Layer<HCons<WorkerHandle, HNil>, Never, R>;              // scoped; drains on close
  pub fn run_until_idle<I>(self) -> Effect<DrainReport, JobError, R>;               // tests, generators
}
pub trait JobObserver: Send + Sync { fn claimed(&self, _: &JobContext) {} fn finished(&self, _: &JobContext,
  _: &Outcome, _: Duration) {} fn lost(&self, _: JobId) {} }    // metrics/console hook; no-op default
```
- Per queue: one poll fiber + a core `Semaphore` of `concurrency` permits; it claims `min(free permits,
  32)` rows, forks each run as a child fiber (scope of `run`), polls again at once if the batch was full,
  else sleeps `poll_interval` (executor clock: virtual under `TestExecutor`). Process-local wake: an
  in-process `enqueue` on the same store notifies the poll fiber (no DB round-trip).
- **Polling, not LISTEN/NOTIFY.** LISTEN needs a dedicated connection outside `SqlClient` (as
  `PgAdvisoryLease` does), breaks under PgBouncer transaction pooling, and is lossy across reconnects
  (polling stays the safety net anyway). The partial dequeue index makes an empty poll an index-only probe;
  1 s latency meets every msg.jobs check. A `wake_on(Stream<()>)` hook keeps NOTIFY additive later.
- Each run: decode → handler with `JobContext` → optional `timeout` → fenced outcome. A defect or panic
  in a handler is a `Retry("defect: …")`; an unknown `name` is a `Retry` (rolling deploys). A store
  error in the poll loop is logged and retried with `Schedule::exponential(100ms)` capped at 30 s; it never
  ends the worker. Each run is wrapped in a span `job.run` {job.name, job.queue, job.id, job.attempt}
  through the same hook E4.3 uses for `db.query`; the observer feeds counters and the console.
- Shutdown: stop claiming, wait up to `drain_timeout` for in-flight runs, interrupt the rest, then
  `release` (attempt refunded); the release runs in a finalizer so interrupting `run` drains too.

## 6. Hooks for later leaves

- **deliver_later (E7.3, `id_effect_mail/src/later.rs`):** `#[derive(Schema)] struct SendMail` +
  `impl Job { NAME = "id_effect_mail.send", QUEUE = "mail" }`; `mail::register(registry)` adds the handler
  (needs `MailerService` in `R`); `deliver_later` = `enqueue`, `deliver_later_in(&tx, m)` = `enqueue_in`.
- **cron (`r10-e7-1-cron`):** each tick enqueues with `UniqueKey { key: "{name}@{tick RFC 3339}", scope:
  UntilPruned }`, so two schedulers (or a leader handover mid-tick) insert once even after the first run
  finished; leader via `ClusterSingleton` over `PgAdvisoryLease`. Needs the fire time in `make` (Q5).
- **outbox relay (E7.2):** reuses the *pattern* (claim CTE + SKIP LOCKED, fenced update, schedule replay),
  not the crate: D5 publish order keeps `id_effect_messaging` independent of `id_effect_jobs`.
- **console (`r10-e6-2-jobs-panel`, stretch #7):** `stats`, `list(JobFilter{state: Dead, ..})`,
  `retry_now`, `cancel`, plus `JobObserver` for live counts. No further API needed.
- **`generate job` (E1.3r):** writes `src/jobs/<name>.rs` (derive(Schema) struct, `impl Job`, `pub fn
  handle(job, ctx) -> Effect<(), JobFailure, AppEnv>`), one `.register::<T, _>(…::handle)?` line in
  `src/jobs/mod.rs`, and a test using `MemoryJobQueue` + `Worker::run_until_idle`; `--cron` adds a cron entry.

## 7. Tests (gate `<pg> -p id_effect_jobs --all-features`)

A shared suite `tests/conformance.rs` runs every backend-neutral case against `MemoryJobQueue`
(TestExecutor + anchored `WallClock`) and `PgJobQueue` (anchored `WallClock` too: `$now` is bound, so Pg
cases are deterministic). Pg-only cases in `tests/pg_worker.rs`.

| Test | Backend | Proves |
|---|---|---|
| `skip_locked_two_workers_never_double_run` (2 workers, 200 jobs, `runs` table PK) | Pg | #1, #6 base |
| `retry_backoff_then_dead` (exponential 1 s, max 3: run_at +1 s, +2 s, then dead, last_error) | both | #2 |
| `unique_key_dedups_while_pending` (+ allowed again after success; UntilPruned blocks) | both | #3 |
| `priority_order_within_queue` (+ two queues isolated) | both | #4 |
| `run_at_respected_under_test_clock` (not claimed at t-1 s, claimed at t) | both | #5 |
| `enqueue_in_rolls_back_with_tx` (rollback: 0 rows; commit: 1; duplicate does not abort tx) | Pg | #8 |
| `per_queue_concurrency_limit` (limit 2: observer never sees 3 in flight; other queue unaffected) | Memory | #6 |
| `crashed_worker_job_is_requeued` (claim, no heartbeat, advance past lease, rescue, rerun; stale complete is `Lost`) | both | #1 |

Plus unit tests: schedule replay, payload decode failure => dead, drain releases with attempt refunded,
`ensure_schema` statement list (auth pattern). #1 durability is evidenced by the Pg table plus the rescue
test; #7 is not claimed. Owns amendment: the leaf also needs `src/lib.rs` (mods, re-exports),
`Cargo.toml` (id_effect_sql `pg`+`migrate`, serde_json, jiff) and the two test files above
(`tests/pg_queue.rs` stays with `r10-e7-1-docs-labs`).

## 8. Open questions (each with a recommendation)

**Decided 2026-10-17 by the owner:** Q1 keep the state name `scheduled` and add `cancelled`;
Q2, Q3, Q4 and Q5 as recommended below.

1. **State names.** Spec text says `queued`; the skeleton says `scheduled`. *Rec:* keep `scheduled`
   (already in `JobState`, Oban term) and add `JobState::Cancelled` (`cancel` needs a terminal state
   distinct from `dead`); the crate is unreleased, so no break.
2. **Unique predicate includes `failed`.** Spec says queued/running only; that lets a duplicate in while
   the first awaits retry. *Rec:* include `failed` and add scope `pruned` (needed for cron correctness).
3. **Migration shipping.** *Rec:* own tracking table `_id_effect_jobs_migrations` via `migrator()` +
   `ensure_schema()`; document copying the file as the alternative. Confirm, since auth did not do this.
4. **Clock source.** *Rec:* bind `$now` from `WallClock` (deterministic tests under an anchored clock,
   one code path with memory) over DB `now()`; cost: node clock skew must stay well below `lease` (doc).
5. **Cron fire time.** `CronJob::new` takes `Fn() -> Effect` without the tick, so `name@tick` cannot be
   built. *Rec:* the cron leaf adds a core `CronJob::new_at(name, cron, Fn(ZonedDateTime) -> Effect)`
   (additive, `id_effect` crate) rather than recomputing the tick from `WallClock` in jobs.

Non-goals: global (cross-node) concurrency limits and rate limiting beyond per-worker permits; LISTEN/NOTIFY;
MySQL/SQLite stores; job batches/workflows (0.11 E7.5); the dashboard (#7, console stretch leaf).
