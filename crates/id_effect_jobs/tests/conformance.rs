//! The backend-neutral queue suite, run against `MemoryJobQueue` on a `TestExecutor` (virtual
//! time) and `PgJobQueue` on a live PostgreSQL (an anchored `WallClock`, moved with `jump`; the
//! queue binds every time from it, so the Pg cases are deterministic up to the real time a test
//! takes). `per_queue_concurrency_limit` runs on the memory backend only.
//!
//! The Pg tests need a database at `DATABASE_URL`. When CI provides a Postgres service it also
//! sets `ID_EFFECT_REQUIRE_PG=1`, which turns a missing or unreachable database into a test
//! **failure**; without it those tests print an explicit `SKIP` line to stderr and return. Each Pg
//! test runs in a schema of its own (`search_path`), so tests running in parallel processes never
//! see each other's jobs or rescue each other's leases.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use id_effect::{
  Cause, Effect, Executor, Exit, Never, Schedule, Schema, TestExecutor, UtcDateTime, WallClock,
  fail, hlist, sleep, succeed,
};
use id_effect_jobs::{
  Claimed, Enqueued, Fenced, Job, JobContext, JobError, JobFailure, JobFilter, JobId, JobObserver,
  JobOptions, JobQueueEnv, JobQueueExt, JobQueueService, JobRecord, JobRegistry, JobState,
  MemoryJobQueue, NewJob, Outcome, Priority, Queue, RunAt, UniqueKey, Worker, WorkerConfig,
  WorkerId,
};

/// 2030-01-01T00:00:00Z: where every test's clock starts.
const T0_MS: i64 = 1_893_456_000_000;

#[derive(Clone, Schema)]
struct Task {
  n: i64,
}

impl Job for Task {
  const NAME: &'static str = "conformance.task";
}

/// Always fails: 3 attempts, 1 s then 2 s apart.
#[derive(Clone, Schema)]
struct Flaky {
  n: i64,
}

impl Job for Flaky {
  const NAME: &'static str = "conformance.flaky";

  fn max_attempts(&self) -> u16 {
    3
  }

  fn retry() -> Schedule<(), JobFailure> {
    Schedule::exponential(Duration::from_secs(1)).map(|_| ())
  }
}

fn registry() -> JobRegistry<JobQueueEnv> {
  JobRegistry::new()
    .register::<Task, Never>(|_, _| succeed(()))
    .and_then(|r| r.register::<Flaky, String>(|job, _| fail(format!("flaky {}", job.n))))
    .expect("distinct names")
}

/// A backend under test.
trait Harness {
  fn store(&self) -> JobQueueService;
  fn clock(&self) -> WallClock;
  /// Runs `eff` to completion; panics on a failure.
  fn run<A: Send + 'static>(&self, eff: Effect<A, JobError>) -> A;
  /// Moves the queue's clock forward.
  fn advance(&self, by: Duration);
  /// How far two instants the test expects equal may differ (real time passing on Pg).
  fn slack(&self) -> Duration;

  fn now(&self) -> UtcDateTime {
    self.run(self.clock().now())
  }

  fn record(&self, id: JobId) -> JobRecord {
    self
      .run(self.store().list(JobFilter::default()))
      .into_iter()
      .find(|r| r.id == id)
      .unwrap_or_else(|| panic!("no job {id}"))
  }

  fn enqueue<J: Job>(&self, job: J, opts: JobOptions) -> Enqueued {
    self.run(self.store().enqueue(job, opts))
  }

  fn claim(&self, queue: &str, worker: &str, limit: u16) -> Vec<Claimed> {
    let store = self.store();
    self.run(store.claim(&Queue::new(queue), worker, limit, Duration::from_secs(30)))
  }

  /// A worker on `queues` drained until nothing is runnable.
  fn drain(&self, queues: &[&str]) -> id_effect_jobs::DrainReport {
    let config = WorkerConfig {
      id: WorkerId::new("conformance"),
      clock: self.clock(),
      prune_after: None,
      ..WorkerConfig::default()
    };
    let worker = queues
      .iter()
      .fold(Worker::new(registry()), |w, q| w.queue(q, 4))
      .with_config(config);
    self.run(worker.run_until_idle().provide_all(hlist![self.store()]))
  }

  fn assert_about(&self, actual: Duration, expected: Duration) {
    let off = actual.abs_diff(expected);
    assert!(
      off <= self.slack(),
      "{actual:?} is not {expected:?} (± {:?})",
      self.slack()
    );
  }
}

fn new_id(e: Enqueued) -> JobId {
  match e {
    Enqueued::New(id) => id,
    dup => panic!("expected a new job, got {dup:?}"),
  }
}

fn payload_n(c: &Claimed) -> i64 {
  c.payload["n"].as_i64().expect("n")
}

/// The cases every backend passes.
mod suite {
  use super::*;

  pub fn retry_backoff_then_dead(h: &impl Harness) {
    let q = "retry";
    let id = new_id(h.enqueue(Flaky { n: 1 }, JobOptions::new().queue(q)));
    // Attempt 1 fails: runnable again 1 s later.
    assert_eq!(h.drain(&[q]).retrying, 1);
    let r = h.record(id);
    assert_eq!((r.state, r.attempts), (JobState::Failed, 1));
    assert_eq!(r.last_error.as_deref(), Some("flaky 1"));
    h.assert_about(
      r.attempted_at.unwrap().distance_duration(&r.run_at),
      Duration::from_secs(1),
    );
    assert_eq!(h.drain(&[q]).runs(), 0, "not before its run_at");
    // Attempt 2 fails: 2 s later.
    h.advance(Duration::from_secs(1));
    assert_eq!(h.drain(&[q]).retrying, 1);
    let r = h.record(id);
    assert_eq!((r.state, r.attempts), (JobState::Failed, 2));
    h.assert_about(
      r.attempted_at.unwrap().distance_duration(&r.run_at),
      Duration::from_secs(2),
    );
    // Attempt 3 of 3 fails: dead, with the last error kept.
    h.advance(Duration::from_secs(2));
    assert_eq!(h.drain(&[q]).dead, 1);
    let r = h.record(id);
    assert_eq!((r.state, r.attempts), (JobState::Dead, 3));
    assert_eq!(r.last_error.as_deref(), Some("flaky 1"));
    assert!(r.finished_at.is_some() && r.locked_by.is_none());
    h.advance(Duration::from_secs(3600));
    assert_eq!(h.drain(&[q]).runs(), 0, "dead jobs never run again");
    // A manual retry starts over.
    assert!(h.run(h.store().retry_now(id)));
    let r = h.record(id);
    assert_eq!((r.state, r.attempts), (JobState::Scheduled, 0));
  }

  pub fn retry_now_brings_a_failed_attempt_forward(h: &impl Harness) {
    let q = "retry-now";
    let id = new_id(h.enqueue(Flaky { n: 1 }, JobOptions::new().queue(q)));
    assert_eq!(h.drain(&[q]).retrying, 1);
    assert_eq!(h.drain(&[q]).runs(), 0, "backing off for 1 s");
    // Runnable now, without the clock moving; its attempts and error are kept.
    assert!(h.run(h.store().retry_now(id)));
    let r = h.record(id);
    assert_eq!((r.state, r.attempts), (JobState::Failed, 1));
    assert_eq!(r.last_error.as_deref(), Some("flaky 1"));
    h.assert_about(h.now().distance_duration(&r.run_at), Duration::ZERO);
    assert_eq!(h.drain(&[q]).retrying, 1, "attempt 2 ran");
    assert_eq!(h.record(id).attempts, 2);
    // A scheduled or succeeded job is left alone.
    let task = new_id(h.enqueue(Task { n: 2 }, JobOptions::new().queue(q)));
    assert!(!h.run(h.store().retry_now(task)));
    assert_eq!(h.drain(&[q]).succeeded, 1);
    assert!(!h.run(h.store().retry_now(task)));
    assert_eq!(h.record(task).state, JobState::Succeeded);
  }

  pub fn unique_key_dedups_while_pending(h: &impl Harness) {
    let (q, other) = ("unique", "unique-other");
    let key = || {
      JobOptions::new()
        .queue(q)
        .unique(UniqueKey::while_pending("k1"))
    };
    let first = new_id(h.enqueue(Task { n: 1 }, key()));
    assert_eq!(
      h.enqueue(Task { n: 2 }, key()),
      Enqueued::Duplicate {
        existing: Some(first)
      }
    );
    let elsewhere = JobOptions::new()
      .queue(other)
      .unique(UniqueKey::while_pending("k1"));
    new_id(h.enqueue(Task { n: 3 }, elsewhere));
    assert_eq!(
      h.drain(&[q, other]).succeeded,
      2,
      "the duplicate inserted nothing"
    );
    // Once the holder succeeded, the key is free.
    let again = new_id(h.enqueue(Task { n: 4 }, key()));
    assert_ne!(again, first);

    // A job awaiting its retry still holds its key.
    let retrying = || {
      JobOptions::new()
        .queue(q)
        .unique(UniqueKey::while_pending("k2"))
    };
    let flaky = new_id(h.enqueue(Flaky { n: 5 }, retrying()));
    h.drain(&[q]);
    assert_eq!(h.record(flaky).state, JobState::Failed);
    assert_eq!(
      h.enqueue(Flaky { n: 6 }, retrying()),
      Enqueued::Duplicate {
        existing: Some(flaky)
      }
    );
    // Cancelling frees it.
    assert!(h.run(h.store().cancel(flaky)));
    new_id(h.enqueue(Flaky { n: 7 }, retrying()));

    // `UntilPruned` (cron ticks) holds the key after success, until the row is pruned.
    let tick = || {
      JobOptions::new()
        .queue(q)
        .unique(UniqueKey::until_pruned("tick@2030-01-01T00:00:00Z"))
    };
    let ran = new_id(h.enqueue(Task { n: 8 }, tick()));
    h.drain(&[q]);
    assert_eq!(h.record(ran).state, JobState::Succeeded);
    assert_eq!(
      h.enqueue(Task { n: 9 }, tick()),
      Enqueued::Duplicate {
        existing: Some(ran)
      }
    );
    let now = h.now();
    let cutoff = UtcDateTime(now.0.checked_add(Duration::from_secs(1)).unwrap());
    assert!(h.run(h.store().prune(cutoff)) >= 1);
    new_id(h.enqueue(Task { n: 10 }, tick()));
  }

  pub fn priority_order_within_queue(h: &impl Harness) {
    let (q, other) = ("prio", "prio-other");
    let now = h.now();
    let at = |p: Priority| JobOptions::new().queue(q).priority(p);
    h.enqueue(Task { n: 1 }, at(Priority::LOW));
    h.enqueue(Task { n: 2 }, at(Priority::NORMAL));
    h.enqueue(Task { n: 3 }, at(Priority::HIGH));
    let earlier = UtcDateTime(now.0.checked_sub(Duration::from_secs(10)).unwrap());
    h.enqueue(
      Task { n: 4 },
      at(Priority::NORMAL).run_at(RunAt::At(earlier)),
    );
    h.enqueue(
      Task { n: 9 },
      JobOptions::new().queue(other).priority(Priority(100)),
    );
    let order: Vec<i64> = (0..5)
      .flat_map(|_| h.claim(q, "w", 1))
      .map(|c| payload_n(&c))
      .collect();
    assert_eq!(
      order,
      [3, 4, 2, 1],
      "priority first, then the earlier run_at; the other queue is never claimed"
    );
    let batch = h.claim(other, "w", 10);
    assert_eq!(batch.iter().map(payload_n).collect::<Vec<_>>(), [9]);
    assert_eq!(batch[0].priority, Priority(100));
  }

  pub fn run_at_respected_under_test_clock(h: &impl Harness) {
    // `enqueue_at` / `enqueue_after` use the job's own queue.
    let q = Queue::DEFAULT;
    let t = UtcDateTime(h.now().0.checked_add(Duration::from_secs(10)).unwrap());
    let id = new_id(h.run(h.store().enqueue_at(Task { n: 1 }, t)));
    assert!(h.claim(q, "w", 5).is_empty(), "not at t - 10 s");
    h.advance(Duration::from_secs(9));
    assert!(h.claim(q, "w", 5).is_empty(), "not at t - 1 s");
    h.advance(Duration::from_secs(1));
    let claimed = h.claim(q, "w", 5);
    assert_eq!(
      claimed.iter().map(|c| c.id).collect::<Vec<_>>(),
      [id],
      "at t"
    );
    assert_eq!(claimed[0].run_at, h.record(id).run_at);
    let after = h.run(
      h.store()
        .enqueue_after(Task { n: 3 }, Duration::from_secs(60)),
    );
    let r = h.record(after.id().unwrap());
    h.assert_about(
      r.inserted_at.distance_duration(&r.run_at),
      Duration::from_secs(60),
    );
  }

  pub fn crashed_worker_job_is_requeued(h: &impl Harness) {
    let q = "crash";
    let id = new_id(h.enqueue(Task { n: 1 }, JobOptions::new().queue(q)));
    let claimed = h.claim(q, "crashed", 1);
    assert_eq!((claimed[0].id, claimed[0].attempt), (id, 1));
    assert_eq!(h.run(h.store().rescue_expired()), 0, "the lease is live");
    assert_eq!(h.drain(&[q]).runs(), 0, "a leased job is not claimed twice");
    // The worker never heartbeats again; its 30 s lease runs out.
    h.advance(Duration::from_secs(31));
    assert_eq!(h.run(h.store().rescue_expired()), 1);
    let r = h.record(id);
    assert_eq!(
      (r.state, r.attempts, r.locked_by),
      (JobState::Failed, 1, None)
    );
    assert_eq!(r.last_error.as_deref(), Some("lease expired (worker lost)"));
    // Another worker runs it again; the crashed worker's late outcome is dropped.
    assert_eq!(h.drain(&[q]).succeeded, 1);
    let r = h.record(id);
    assert_eq!((r.state, r.attempts), (JobState::Succeeded, 2));
    assert_eq!(h.run(h.store().complete(id, "crashed")), Fenced::Lost);
    let f = JobFailure::retry("late");
    assert_eq!(h.run(h.store().fail(id, "crashed", &f, None)), Fenced::Lost);
    assert_eq!(h.record(id).state, JobState::Succeeded);

    // Rescued on its last attempt: dead.
    let last = NewJob {
      max_attempts: 1,
      ..NewJob::of(Task { n: 2 }, JobOptions::new().queue(q))
    };
    let id = new_id(h.run(h.store().enqueue_raw(last)));
    assert_eq!(h.claim(q, "crashed", 1).len(), 1);
    h.advance(Duration::from_secs(31));
    assert_eq!(h.run(h.store().rescue_expired()), 1);
    assert_eq!(h.record(id).state, JobState::Dead);
  }
}

// ── memory ──────────────────────────────────────────────────────────────────────────────────

struct Memory {
  exec: TestExecutor,
  clock: WallClock,
  store: MemoryJobQueue,
}

impl Memory {
  fn new() -> Self {
    let exec = TestExecutor::new();
    let clock = WallClock::anchored(exec.now(), UtcDateTime::unsafe_make(T0_MS));
    Self {
      store: MemoryJobQueue::new(clock.clone()),
      clock,
      exec,
    }
  }
}

impl Harness for Memory {
  fn store(&self) -> JobQueueService {
    JobQueueService::new(self.store.clone())
  }

  fn clock(&self) -> WallClock {
    self.clock.clone()
  }

  fn run<A: Send + 'static>(&self, eff: Effect<A, JobError>) -> A {
    let slot = Arc::new(Mutex::new(None));
    let out = Arc::clone(&slot);
    let h = eff
      .exit::<Never>()
      .map(move |exit| *out.lock().unwrap() = Some(exit))
      .fork_root(Arc::new(self.exec.clone()));
    self.exec.run_until_stalled();
    assert!(h.is_done(), "the effect waits on time");
    match slot.lock().unwrap().take() {
      Some(Exit::Success(a)) => a,
      Some(Exit::Failure(c)) => panic!("failed: {c:?}"),
      None => panic!("no exit"),
    }
  }

  fn advance(&self, by: Duration) {
    self.exec.advance(by);
  }

  fn slack(&self) -> Duration {
    Duration::ZERO
  }
}

mod memory {
  use super::*;

  #[test]
  fn retry_backoff_then_dead() {
    suite::retry_backoff_then_dead(&Memory::new());
  }

  #[test]
  fn retry_now_brings_a_failed_attempt_forward() {
    suite::retry_now_brings_a_failed_attempt_forward(&Memory::new());
  }

  #[test]
  fn unique_key_dedups_while_pending() {
    suite::unique_key_dedups_while_pending(&Memory::new());
  }

  #[test]
  fn priority_order_within_queue() {
    suite::priority_order_within_queue(&Memory::new());
  }

  #[test]
  fn run_at_respected_under_test_clock() {
    suite::run_at_respected_under_test_clock(&Memory::new());
  }

  #[test]
  fn crashed_worker_job_is_requeued() {
    suite::crashed_worker_job_is_requeued(&Memory::new());
  }

  /// Counts the attempts in flight per queue and the most seen at once.
  #[derive(Default)]
  struct InFlight {
    now: Mutex<std::collections::BTreeMap<String, (usize, usize)>>,
    done: AtomicUsize,
  }

  impl InFlight {
    fn max(&self, queue: &str) -> usize {
      self.now.lock().unwrap().get(queue).map_or(0, |e| e.1)
    }
  }

  impl JobObserver for InFlight {
    fn claimed(&self, job: &JobContext) {
      let mut now = self.now.lock().unwrap();
      let e = now.entry(job.queue.to_string()).or_default();
      e.0 += 1;
      e.1 = e.1.max(e.0);
    }

    fn finished(&self, job: &JobContext, outcome: &Outcome, _: Duration) {
      assert_eq!(*outcome, Outcome::Succeeded);
      self
        .now
        .lock()
        .unwrap()
        .get_mut(job.queue.as_str())
        .unwrap()
        .0 -= 1;
      self.done.fetch_add(1, Ordering::SeqCst);
    }
  }

  #[derive(Clone, Schema)]
  struct Slow {
    n: i64,
  }

  impl Job for Slow {
    const NAME: &'static str = "conformance.slow";
  }

  #[test]
  fn per_queue_concurrency_limit() {
    let h = Memory::new();
    for n in 0..6 {
      h.enqueue(Slow { n }, JobOptions::new().queue("a"));
    }
    for n in 0..2 {
      h.enqueue(Slow { n }, JobOptions::new().queue("b"));
    }
    let registry = JobRegistry::<JobQueueEnv>::new()
      .register::<Slow, Never>(|_, _| sleep(Duration::from_secs(10)))
      .unwrap();
    let observer = Arc::new(InFlight::default());
    let config = WorkerConfig {
      id: WorkerId::new("limited"),
      clock: h.clock(),
      observer: observer.clone(),
      ..WorkerConfig::default()
    };
    let worker = Worker::new(registry)
      .queue("a", 2)
      .queue("b", 1)
      .with_config(config);
    let fiber = worker
      .run()
      .provide_all(hlist![h.store()])
      .fork_root(Arc::new(h.exec.clone()));
    h.exec.advance(Duration::from_secs(1));
    assert_eq!(observer.max("a"), 2, "queue a is limited to 2");
    assert_eq!(observer.max("b"), 1);
    // Queue b is not starved by a: its first job finishes while a still has work.
    h.exec.advance(Duration::from_secs(10));
    assert_eq!(observer.done.load(Ordering::SeqCst), 3);
    h.exec.advance(Duration::from_secs(30));
    assert_eq!(observer.done.load(Ordering::SeqCst), 8);
    assert_eq!(
      (observer.max("a"), observer.max("b")),
      (2, 1),
      "never more in flight"
    );
    let stats = h.run(h.store().stats());
    assert!(
      stats
        .iter()
        .all(|s| s.count(JobState::Succeeded) == s.counts.values().sum::<u64>())
    );
    fiber.interrupt_now();
    h.exec.advance(Duration::from_secs(1));
    assert!(fiber.is_done());
  }
}

// ── PostgreSQL ──────────────────────────────────────────────────────────────────────────────

mod pg {
  use id_effect_jobs::{PgJobQueue, migrator};
  use id_effect_sql::pg::{PgPoolConfig, PgSqlClient, pg_pool_from_config};
  use id_effect_sql::{SqlClient, SqlError};
  use id_effect_tokio::TokioRuntime;

  use super::*;

  fn pg_required() -> bool {
    std::env::var("ID_EFFECT_REQUIRE_PG").is_ok_and(|v| v == "1")
  }

  /// Fail (when `ID_EFFECT_REQUIRE_PG=1`) or loudly skip a live test that has no database.
  fn unavailable<T>(test: &str, why: &str) -> Option<T> {
    assert!(
      !pg_required(),
      "{test}: ID_EFFECT_REQUIRE_PG=1 but Postgres is unavailable: {why}"
    );
    eprintln!("SKIP {test}: {why} (set DATABASE_URL, and ID_EFFECT_REQUIRE_PG=1 to require it)");
    None
  }

  pub(super) struct Pg {
    rt: TokioRuntime,
    clock: WallClock,
    store: PgJobQueue,
  }

  impl Pg {
    fn sql<A: Send + 'static>(rt: &TokioRuntime, eff: Effect<A, SqlError>) -> Result<A, String> {
      match rt.block_on(rt.run(eff)) {
        Exit::Success(a) => Ok(a),
        Exit::Failure(c) => Err(format!("{c:?}")),
      }
    }

    /// A queue in the fresh schema `jobs_<test>`, migrated, on a clock anchored at 2030-01-01.
    pub(super) fn new(test: &str) -> Option<Self> {
      let url = match std::env::var("DATABASE_URL") {
        Ok(url) => url,
        Err(_) => return unavailable(test, "DATABASE_URL is not set"),
      };
      let rt = TokioRuntime::new_multi_thread().expect("tokio runtime");
      let schema = format!("jobs_{test}");
      let admin = match rt.block_on(pg_pool_from_config(
        PgPoolConfig::from_url(url.clone()).with_max_size(1),
      )) {
        Ok(pool) => PgSqlClient::new(pool),
        Err(e) => return unavailable(test, &format!("cannot connect: {}", e.0)),
      };
      for ddl in [
        format!("DROP SCHEMA IF EXISTS {schema} CASCADE"),
        format!("CREATE SCHEMA {schema}"),
      ] {
        if let Err(e) = Self::sql(&rt, admin.execute(&ddl, &[])) {
          return unavailable(test, &e);
        }
      }
      let sep = if url.contains('?') { '&' } else { '?' };
      let scoped = format!("{url}{sep}options=-c%20search_path%3D{schema}");
      let pool = rt
        .block_on(pg_pool_from_config(
          PgPoolConfig::from_url(scoped).with_max_size(8),
        ))
        .expect("pool on the test schema");
      let client = PgSqlClient::new(pool);
      let applied = rt.block_on(rt.run(migrator().run_on(Arc::new(client.clone()))));
      assert!(
        matches!(&applied, Exit::Success(v) if *v == [1]),
        "migrate: {applied:?}"
      );
      let clock = WallClock::anchored(rt.now(), UtcDateTime::unsafe_make(T0_MS));
      let store = PgJobQueue::new(client).with_clock(clock.clone());
      Some(Self { rt, clock, store })
    }
  }

  impl Harness for Pg {
    fn store(&self) -> JobQueueService {
      JobQueueService::new(self.store.clone())
    }

    fn clock(&self) -> WallClock {
      self.clock.clone()
    }

    fn run<A: Send + 'static>(&self, eff: Effect<A, JobError>) -> A {
      match self.rt.block_on(self.rt.run(eff)) {
        Exit::Success(a) => a,
        Exit::Failure(Cause::Fail(e)) => panic!("failed: {e}"),
        Exit::Failure(c) => panic!("failed: {c:?}"),
      }
    }

    fn advance(&self, by: Duration) {
      self.clock.jump(by);
    }

    fn slack(&self) -> Duration {
      Duration::from_secs(2)
    }
  }

  #[test]
  fn retry_backoff_then_dead() {
    if let Some(h) = Pg::new("retry_backoff_then_dead") {
      suite::retry_backoff_then_dead(&h);
    }
  }

  #[test]
  fn retry_now_brings_a_failed_attempt_forward() {
    if let Some(h) = Pg::new("retry_now_brings_a_failed_attempt_forward") {
      suite::retry_now_brings_a_failed_attempt_forward(&h);
    }
  }

  #[test]
  fn unique_key_dedups_while_pending() {
    if let Some(h) = Pg::new("unique_key_dedups_while_pending") {
      suite::unique_key_dedups_while_pending(&h);
    }
  }

  #[test]
  fn priority_order_within_queue() {
    if let Some(h) = Pg::new("priority_order_within_queue") {
      suite::priority_order_within_queue(&h);
    }
  }

  #[test]
  fn run_at_respected_under_test_clock() {
    if let Some(h) = Pg::new("run_at_respected_under_test_clock") {
      suite::run_at_respected_under_test_clock(&h);
    }
  }

  #[test]
  fn crashed_worker_job_is_requeued() {
    if let Some(h) = Pg::new("crashed_worker_job_is_requeued") {
      suite::crashed_worker_job_is_requeued(&h);
    }
  }
}
