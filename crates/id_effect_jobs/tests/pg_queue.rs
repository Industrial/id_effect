//! The queue on PostgreSQL under running workers (`Worker::run`, real time): per-queue
//! concurrency, retries until `dead`, a retry that then succeeds, priority order and a held-back
//! `run_at`, end to end. `conformance.rs` covers the store-level cases on both backends and
//! `pg_worker.rs` `SKIP LOCKED`, `enqueue_in` and the schema.
//!
//! The tests need a database at `DATABASE_URL`. When CI provides a Postgres service it also sets
//! `ID_EFFECT_REQUIRE_PG=1`, which turns a missing or unreachable database into a test
//! **failure**; without it they print an explicit `SKIP` line to stderr and return. Each test runs
//! in a schema of its own (`search_path`), so parallel test processes never share rows.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use id_effect::{Effect, Exit, Never, Schedule, Schema, fail, hlist, sleep, succeed, sync};
use id_effect_jobs::{
  Job, JobContext, JobFailure, JobFilter, JobObserver, JobOptions, JobQueue, JobQueueEnv,
  JobQueueExt, JobQueueService, JobRecord, JobRegistry, JobState, NoopObserver, Outcome,
  PgJobQueue, Priority, Queue, RunAt, Worker, WorkerConfig, WorkerId, migrator,
};
use id_effect_sql::SqlClient;
use id_effect_sql::pg::{PgPoolConfig, PgSqlClient, pg_pool_from_config};
use id_effect_tokio::TokioRuntime;

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

struct Db {
  rt: TokioRuntime,
  client: PgSqlClient,
}

impl Db {
  /// A client on the fresh, migrated schema `jobs_<test>`.
  fn new(test: &str) -> Option<Self> {
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
    let fresh = admin
      .execute(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE"), &[])
      .flat_map(move |_| admin.execute(&format!("CREATE SCHEMA {schema}"), &[]));
    if let Exit::Failure(c) = rt.block_on(rt.run(fresh)) {
      return unavailable(test, &format!("{c:?}"));
    }
    let sep = if url.contains('?') { '&' } else { '?' };
    let scoped = format!("{url}{sep}options=-c%20search_path%3Djobs_{test}");
    let pool = rt
      .block_on(pg_pool_from_config(
        PgPoolConfig::from_url(scoped).with_max_size(16),
      ))
      .expect("pool on the test schema");
    let db = Self {
      rt,
      client: PgSqlClient::new(pool),
    };
    let applied = db.run(
      migrator()
        .run_on(Arc::new(db.client.clone()))
        .map_err(|e| e.to_string()),
    );
    assert_eq!(applied, [1]);
    Some(db)
  }

  fn run<A: Send + 'static, E: std::fmt::Debug + Send + 'static>(&self, eff: Effect<A, E>) -> A {
    match self.rt.block_on(self.rt.run(eff)) {
      Exit::Success(a) => a,
      Exit::Failure(c) => panic!("failed: {c:?}"),
    }
  }

  /// The jobs of `queue`, newest first.
  fn jobs(&self, store: &PgJobQueue, queue: &str) -> Vec<JobRecord> {
    let filter = JobFilter {
      queue: Some(Queue::new(queue)),
      ..JobFilter::default()
    };
    self.run(store.list(filter))
  }

  /// Runs `worker` on its own queue handle until `done` holds (at most 60 s), then stops it.
  fn run_worker_until(
    &self,
    worker: Worker<JobQueueEnv>,
    mut done: impl FnMut(&Db) -> bool,
  ) -> bool {
    let env = hlist![JobQueueService::new(PgJobQueue::new(self.client.clone()))];
    let fiber = self.rt.fork(worker.run().provide_all(env));
    let deadline = Instant::now() + Duration::from_secs(60);
    let reached = loop {
      if done(self) {
        break true;
      }
      if Instant::now() > deadline {
        break false;
      }
      std::thread::sleep(Duration::from_millis(50));
    };
    fiber.interrupt_now();
    assert!(matches!(
      self.rt.block_on(fiber.join_future()),
      Exit::Failure(_)
    ));
    reached
  }
}

fn config(id: &str, observer: Arc<dyn JobObserver>) -> WorkerConfig {
  WorkerConfig {
    id: WorkerId::new(id),
    poll_interval: Duration::from_millis(20),
    observer,
    ..WorkerConfig::default()
  }
}

fn succeeded(db: &Db, store: &PgJobQueue) -> u64 {
  db.run(store.stats())
    .iter()
    .map(|s| s.count(JobState::Succeeded))
    .sum()
}

/// Counts the attempts in flight per queue and the most seen at once.
#[derive(Default)]
struct InFlight {
  now: Mutex<BTreeMap<String, (usize, usize)>>,
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

  fn finished(&self, job: &JobContext, _: &Outcome, _: Duration) {
    if let Some(e) = self.now.lock().unwrap().get_mut(job.queue.as_str()) {
      e.0 -= 1;
    }
  }
}

#[derive(Clone, Schema)]
struct Slow {
  n: i64,
}

impl Job for Slow {
  const NAME: &'static str = "pg_queue.slow";
}

#[test]
fn per_queue_concurrency_limit_on_pg() {
  let test = "per_queue_concurrency_limit_on_pg";
  let Some(db) = Db::new(test) else {
    return;
  };
  let store = PgJobQueue::new(db.client.clone());
  for n in 0..6 {
    db.run(store.enqueue(Slow { n }, JobOptions::new().queue("a")));
  }
  for n in 0..3 {
    db.run(store.enqueue(Slow { n }, JobOptions::new().queue("b")));
  }
  let registry = JobRegistry::<JobQueueEnv>::new()
    .register::<Slow, Never>(|_, _| sleep(Duration::from_millis(200)))
    .unwrap();
  let observer = Arc::new(InFlight::default());
  let worker = Worker::new(registry)
    .queue("a", 2)
    .queue("b", 1)
    .with_config(config("limited", observer.clone()));

  let done = db.run_worker_until(worker, |db| succeeded(db, &store) == 9);
  assert!(done, "every job succeeded within 60 s");
  assert_eq!(observer.max("a"), 2, "queue a runs 2 at a time, never more");
  assert_eq!(observer.max("b"), 1, "queue b runs 1 at a time");
}

/// Records the order the `ordered` queue runs in.
#[derive(Clone, Schema)]
struct Ordered {
  n: i64,
}

impl Job for Ordered {
  const NAME: &'static str = "pg_queue.ordered";
  const QUEUE: &'static str = "ordered";
}

/// Always fails: 3 attempts, 50 ms apart.
#[derive(Clone, Schema)]
struct Flaky {
  n: i64,
}

impl Job for Flaky {
  const NAME: &'static str = "pg_queue.flaky";
  const QUEUE: &'static str = "retry";

  fn max_attempts(&self) -> u16 {
    3
  }

  fn retry() -> Schedule<(), JobFailure> {
    Schedule::spaced(Duration::from_millis(50)).map(|_| ())
  }
}

/// Fails its first attempt, then succeeds.
#[derive(Clone, Schema)]
struct SecondTime {
  n: i64,
}

impl Job for SecondTime {
  const NAME: &'static str = "pg_queue.second_time";
  const QUEUE: &'static str = "retry";

  fn retry() -> Schedule<(), JobFailure> {
    Schedule::spaced(Duration::from_millis(50)).map(|_| ())
  }
}

#[test]
fn running_worker_retries_orders_and_defers_on_pg() {
  let test = "running_worker_retries_orders_and_defers_on_pg";
  let Some(db) = Db::new(test) else {
    return;
  };
  let store = PgJobQueue::new(db.client.clone());
  // Enqueued before the worker starts: it claims by priority, not by insertion.
  for (n, priority) in [
    (1, Priority::LOW),
    (3, Priority::HIGH),
    (2, Priority::NORMAL),
  ] {
    db.run(store.enqueue(Ordered { n }, JobOptions::new().priority(priority)));
  }
  // Highest priority, but not due for an hour.
  let later = JobOptions::new()
    .priority(Priority(100))
    .run_at(RunAt::After(Duration::from_secs(3600)));
  db.run(store.enqueue(Ordered { n: 99 }, later));
  db.run(store.enqueue(Flaky { n: 1 }, JobOptions::new()));
  db.run(store.enqueue(SecondTime { n: 1 }, JobOptions::new()));

  let seen = Arc::new(Mutex::new(Vec::new()));
  let s = Arc::clone(&seen);
  let registry = JobRegistry::<JobQueueEnv>::new()
    .register::<Ordered, Never>(move |job, _| {
      let s = Arc::clone(&s);
      sync(move || s.lock().unwrap().push(job.n))
    })
    .and_then(|r| r.register::<Flaky, String>(|job, _| fail(format!("flaky {}", job.n))))
    .and_then(|r| {
      r.register::<SecondTime, String>(|_, ctx| {
        if ctx.attempt == 1 {
          fail("not yet".to_owned())
        } else {
          succeed(())
        }
      })
    })
    .unwrap();
  let worker = Worker::new(registry)
    .queue("ordered", 1)
    .queue("retry", 2)
    .with_config(config("lifecycle", Arc::new(NoopObserver)));

  let settled = |db: &Db| {
    let retry = db.jobs(&store, "retry");
    seen.lock().unwrap().len() == 3 && retry.iter().all(|j| j.state.is_terminal())
  };
  assert!(db.run_worker_until(worker, settled), "settled within 60 s");

  assert_eq!(*seen.lock().unwrap(), [3, 2, 1], "higher priority first");
  let ordered = db.jobs(&store, "ordered");
  let held = ordered
    .iter()
    .find(|j| j.priority == Priority(100))
    .unwrap();
  assert_eq!((held.state, held.attempts), (JobState::Scheduled, 0));

  let retry = db.jobs(&store, "retry");
  let by_name = |name: &str| retry.iter().find(|j| j.name == name).unwrap();
  let dead = by_name(Flaky::NAME);
  assert_eq!((dead.state, dead.attempts), (JobState::Dead, 3));
  assert_eq!(dead.last_error.as_deref(), Some("flaky 1"));
  assert!(dead.finished_at.is_some() && dead.locked_by.is_none());
  let retried = by_name(SecondTime::NAME);
  assert_eq!((retried.state, retried.attempts), (JobState::Succeeded, 2));
}
