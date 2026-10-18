//! PostgreSQL-only queue behaviour: `SKIP LOCKED` under two concurrent workers, `enqueue_in` on
//! the caller's transaction, and the shipped schema.
//!
//! The tests need a database at `DATABASE_URL`. When CI provides a Postgres service it also sets
//! `ID_EFFECT_REQUIRE_PG=1`, which turns a missing or unreachable database into a test
//! **failure**; without it they print an explicit `SKIP` line to stderr and return. Each test runs
//! in a schema of its own (`search_path`), so parallel test processes never share rows.

use std::sync::Arc;
use std::time::{Duration, Instant};

use id_effect::{Effect, Exit, Schema, UtcDateTime, WallClock, hlist};
use id_effect_jobs::{
  Enqueued, Job, JobError, JobOptions, JobQueue, JobQueueEnv, JobQueueExt, JobQueueService,
  JobRegistry, JobState, PgJobQueue, UniqueKey, Worker, WorkerConfig, WorkerId, enqueue_in,
  migrator,
};
use id_effect_sql::pg::{PgPoolConfig, PgSqlClient, pg_pool_from_config};
use id_effect_sql::{SqlClient, SqlError, SqlValue, params};
use id_effect_tokio::TokioRuntime;

const DOWN: &str = include_str!("../migrations/0001_id_effect_jobs.down.sql");

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

  fn texts(&self, sql: &str, params: &[SqlValue]) -> Vec<String> {
    self
      .run(self.client.query(sql, params))
      .iter()
      .map(|r| r.get::<String>(0usize).expect("a text column"))
      .collect()
  }

  fn count(&self, sql: &str, params: &[SqlValue]) -> i64 {
    self.run(self.client.query(sql, params))[0]
      .get::<i64>(0usize)
      .expect("a count")
  }
}

#[derive(Clone, Schema)]
struct Task {
  n: i64,
}

impl Job for Task {
  const NAME: &'static str = "pg.task";
}

#[test]
fn skip_locked_two_workers_never_double_run() {
  let test = "skip_locked_two_workers_never_double_run";
  let Some(db) = Db::new(test) else {
    return;
  };
  db.run(
    db.client
      .execute("CREATE TABLE runs (job_id bigint PRIMARY KEY)", &[]),
  );
  let store = PgJobQueue::new(db.client.clone());
  for n in 0..200 {
    db.run(store.enqueue(Task { n }, JobOptions::new()));
  }
  // Each run inserts its job id: a second run of a job would violate the primary key.
  let client = db.client.clone();
  let registry = JobRegistry::<JobQueueEnv>::new()
    .register::<Task, SqlError>(move |_, ctx| {
      client
        .execute("INSERT INTO runs (job_id) VALUES ($1)", &params![ctx.id.0])
        .map(|_| ())
        .narrow()
    })
    .unwrap();
  let workers: Vec<_> = ["worker-a", "worker-b"]
    .into_iter()
    .map(|id| {
      let config = WorkerConfig {
        id: WorkerId::new(id),
        poll_interval: Duration::from_millis(20),
        ..WorkerConfig::default()
      };
      // Each worker has its own queue handle (no shared in-process wake-ups).
      let env = hlist![JobQueueService::new(PgJobQueue::new(db.client.clone()))];
      let worker = Worker::new(registry.clone())
        .queue("default", 8)
        .with_config(config);
      db.rt.fork(worker.run().provide_all(env))
    })
    .collect();

  let deadline = Instant::now() + Duration::from_secs(60);
  let succeeded = loop {
    let stats = db.run(store.stats());
    let done = stats.first().map_or(0, |s| s.count(JobState::Succeeded));
    if done == 200 || Instant::now() > deadline {
      break done;
    }
    std::thread::sleep(Duration::from_millis(50));
  };
  for w in &workers {
    w.interrupt_now();
  }
  for w in workers {
    assert!(matches!(db.rt.block_on(w.join_future()), Exit::Failure(_)));
  }
  assert_eq!(succeeded, 200, "every job succeeded");
  assert_eq!(db.count("SELECT count(*) FROM runs", &[]), 200);
  assert_eq!(
    db.count(
      "SELECT count(*) FROM id_effect_jobs WHERE attempts <> 1 OR state <> 'succeeded'",
      &[]
    ),
    0,
    "no job ran twice or failed"
  );
  let by_worker = db.count(
    "SELECT count(DISTINCT locked_by) FROM id_effect_jobs WHERE locked_by IS NOT NULL",
    &[],
  );
  assert_eq!(by_worker, 0, "finished jobs hold no lease");
}

#[test]
fn enqueue_in_rolls_back_with_tx() {
  let test = "enqueue_in_rolls_back_with_tx";
  let Some(db) = Db::new(test) else {
    return;
  };
  let clock = WallClock::anchored(db.rt.now(), UtcDateTime::unsafe_make(1_893_456_000_000));
  let store = PgJobQueue::new(db.client.clone()).with_clock(clock.clone());
  let count = |db: &Db| db.count("SELECT count(*) FROM id_effect_jobs", &[]);
  let begin = |db: &Db| db.run(db.client.begin());
  let opts = || JobOptions::new().queue("tx");

  // Rolled back: no row.
  let tx = begin(&db);
  let inserted = db.run(enqueue_in(&tx, Task { n: 1 }, opts(), &clock));
  assert!(inserted.is_new());
  assert_eq!(count(&db), 0, "not visible before the commit");
  db.run(tx.rollback());
  assert_eq!(count(&db), 0);

  // Committed: one row, at the clock's time.
  let tx = begin(&db);
  let id = db
    .run(enqueue_in(&tx, Task { n: 2 }, opts(), &clock))
    .id()
    .unwrap();
  db.run(tx.commit());
  assert_eq!(count(&db), 1);
  let at = db.texts(
    "SELECT to_char(inserted_at AT TIME ZONE 'UTC', 'YYYY-MM-DD') FROM id_effect_jobs WHERE id = $1",
    &params![id.0],
  );
  assert_eq!(
    at,
    ["2030-01-01"],
    "times come from the WallClock, not now()"
  );

  // A duplicate key inserts nothing and does not abort the transaction.
  let key = || opts().unique(UniqueKey::while_pending("invoice-7"));
  let holder = db.run(store.enqueue(Task { n: 3 }, key())).id().unwrap();
  let tx = begin(&db);
  let dup = db.run(enqueue_in(&tx, Task { n: 4 }, key(), &clock));
  assert_eq!(
    dup,
    Enqueued::Duplicate {
      existing: Some(holder)
    }
  );
  let still_usable = db.run(tx.query("SELECT 1::bigint AS one", &[]));
  assert_eq!(still_usable[0].get::<i64>("one"), Ok(1));
  let after = db.run(enqueue_in(&tx, Task { n: 5 }, opts(), &clock));
  assert!(after.is_new());
  db.run(tx.commit());
  assert_eq!(count(&db), 3);

  // Bad options never reach the database.
  let tx = begin(&db);
  let empty = JobOptions::new().queue("");
  let rejected = db
    .rt
    .block_on(db.rt.run(enqueue_in(&tx, Task { n: 6 }, empty, &clock)));
  assert!(matches!(
    rejected,
    Exit::Failure(id_effect::Cause::Fail(JobError::InvalidOptions(_)))
  ));
  db.run(tx.rollback());
}

#[test]
fn migrator_and_ensure_schema_agree() {
  let test = "migrator_and_ensure_schema_agree";
  let Some(db) = Db::new(test) else {
    return;
  };
  let store = PgJobQueue::new(db.client.clone());
  // Idempotent over the migrated table.
  db.run(store.ensure_schema());
  let tracked = db.count(
    "SELECT count(*) FROM _id_effect_jobs_migrations WHERE version = 1",
    &[],
  );
  assert_eq!(tracked, 1, "tracked in the crate's own table");
  let columns = || {
    db.texts(
      "SELECT column_name || ':' || data_type FROM information_schema.columns \
       WHERE table_schema = current_schema() AND table_name = 'id_effect_jobs' \
       ORDER BY ordinal_position",
      &[],
    )
  };
  let migrated = columns();
  assert_eq!(migrated.len(), 17);
  assert_eq!(migrated[0], "id:bigint");
  assert!(migrated.contains(&"payload:jsonb".to_owned()));
  assert!(migrated.contains(&"locked_until:timestamp with time zone".to_owned()));
  let indexes = || {
    db.texts(
      "SELECT indexname::text FROM pg_indexes \
       WHERE schemaname = current_schema() AND tablename = 'id_effect_jobs' ORDER BY indexname",
      &[],
    )
  };
  let expected = [
    "id_effect_jobs_dequeue_idx",
    "id_effect_jobs_finished_idx",
    "id_effect_jobs_lease_idx",
    "id_effect_jobs_pkey",
    "id_effect_jobs_unique_idx",
  ];
  assert_eq!(indexes(), expected);
  // The down script drops it; `ensure_schema` alone recreates the same table.
  db.run(db.client.execute(DOWN.trim(), &[]));
  assert!(indexes().is_empty());
  db.run(store.ensure_schema());
  assert_eq!(columns(), migrated);
  assert_eq!(indexes(), expected);
  // The state CHECK knows `cancelled`.
  let cancelled = db.run(store.enqueue(Task { n: 1 }, JobOptions::new()));
  let id = cancelled.id().unwrap();
  assert!(db.run(store.cancel(id)));
  let state = db.texts(
    "SELECT state FROM id_effect_jobs WHERE id = $1",
    &params![id.0],
  );
  assert_eq!(state, [JobState::Cancelled.as_str()]);
}
