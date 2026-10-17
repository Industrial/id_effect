//! Cron on PostgreSQL (feature `cron`): two schedulers enqueue each tick once, and a leader that
//! loses its `PgAdvisoryLease` hands the schedule over to a follower.
//!
//! The tests need a database at `DATABASE_URL` and run in real time (a six-field, every-second
//! expression on the system clock). When CI provides a Postgres service it also sets
//! `ID_EFFECT_REQUIRE_PG=1`, which turns a missing or unreachable database into a test
//! **failure**; without it they print an explicit `SKIP` line to stderr and return. Each test runs
//! in a schema of its own (`search_path`), so parallel test processes never share rows.

#![cfg(feature = "cron")]

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use id_effect::{
  Cron, CronOutcome, CronRegistry, CronScheduler, Effect, Exit, Schema, WallClock, hlist, succeed,
};
use id_effect_jobs::{
  CronLeader, Enqueued, Job, JobFailure, JobOptions, JobQueueEnv, JobQueueExt, JobQueueService,
  JobRegistry, PgJobQueue, Worker, WorkerConfig, WorkerId, enqueue_on_tick, migrator, tick_key,
};
use id_effect_node::singleton::{ClusterSingleton, PgAdvisoryLease};
use id_effect_process::{NodeName, Pid};
use id_effect_sql::pg::{PgPoolConfig, PgSqlClient, pg_pool_from_config};
use id_effect_sql::{SqlClient, SqlValue, params};
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
  url: String,
  client: PgSqlClient,
}

impl Db {
  /// A client on the fresh, migrated schema `cron_<test>`.
  fn new(test: &str) -> Option<Self> {
    let url = match std::env::var("DATABASE_URL") {
      Ok(url) => url,
      Err(_) => return unavailable(test, "DATABASE_URL is not set"),
    };
    let rt = TokioRuntime::new_multi_thread().expect("tokio runtime");
    let schema = format!("cron_{test}");
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
    let scoped = with_param(&url, &format!("options=-c%20search_path%3Dcron_{test}"));
    let pool = rt
      .block_on(pg_pool_from_config(
        PgPoolConfig::from_url(scoped).with_max_size(16),
      ))
      .expect("pool on the test schema");
    let db = Self {
      rt,
      url,
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

  fn count(&self, sql: &str, params: &[SqlValue]) -> i64 {
    self.run(self.client.query(sql, params))[0]
      .get::<i64>(0usize)
      .expect("a count")
  }

  /// `(unique_key, by)` of every job, by tick.
  fn ticks(&self) -> Vec<(String, String)> {
    let sql = "SELECT unique_key, payload->>'by' FROM id_effect_jobs ORDER BY run_at, id";
    self
      .run(self.client.query(sql, &[]))
      .iter()
      .map(|r| {
        let key = r.get::<String>(0usize).expect("unique_key");
        (key, r.get::<String>(1usize).expect("by"))
      })
      .collect()
  }

  fn env(&self) -> JobQueueEnv {
    hlist![JobQueueService::new(PgJobQueue::new(self.client.clone()))]
  }
}

fn with_param(url: &str, param: &str) -> String {
  let sep = if url.contains('?') { '&' } else { '?' };
  format!("{url}{sep}{param}")
}

/// Polls `done` every 20 ms until it holds or `within` passes.
fn wait_until(within: Duration, mut done: impl FnMut() -> bool) -> bool {
  let deadline = Instant::now() + within;
  while !done() {
    if Instant::now() > deadline {
      return false;
    }
    std::thread::sleep(Duration::from_millis(20));
  }
  true
}

#[derive(Clone, Schema)]
struct Beat {
  by: String,
}

impl Job for Beat {
  const NAME: &'static str = "cron_pg.beat";
}

/// The entry `beat`, every second, enqueuing a [`Beat`] signed `by`.
fn beat(by: &'static str) -> CronRegistry<JobQueueEnv> {
  let every_second = Cron::parse("* * * * * *").expect("six-field cron");
  CronRegistry::new()
    .with(enqueue_on_tick("beat", every_second, move |_| Beat {
      by: by.into(),
    }))
    .expect("register")
}

/// The ticks `scheduler` enqueued successfully (inserted or found as duplicates), as keys.
fn succeeded(scheduler: &CronScheduler) -> BTreeSet<String> {
  scheduler
    .history("beat")
    .expect("job")
    .iter()
    .filter(|run| run.outcome == CronOutcome::Succeeded)
    .map(|run| tick_key("beat", &run.scheduled).key)
    .collect()
}

#[test]
fn cron_tick_enqueues_once_across_two_schedulers() {
  let test = "cron_tick_enqueues_once_across_two_schedulers";
  let Some(db) = Db::new(test) else {
    return;
  };
  let (a, b) = (beat("a"), beat("b"));
  let handlers = JobRegistry::<JobQueueEnv>::new()
    .register::<Beat, JobFailure>(|_, _| succeed(()))
    .expect("handler");
  let worker = Worker::new(handlers)
    .queue("default", 4)
    .with_config(WorkerConfig {
      id: WorkerId::new("cron-worker"),
      poll_interval: Duration::from_millis(20),
      ..WorkerConfig::default()
    });
  // Two scheduler processes (own queue handles) on the same entry, and a worker finishing the
  // jobs as they come, so a late scheduler may find its tick already run.
  let fibers = [
    db.rt.fork(a.run(WallClock::system()).provide_all(db.env())),
    db.rt.fork(b.run(WallClock::system()).provide_all(db.env())),
    db.rt.fork(worker.run().provide_all(db.env())),
  ];
  std::thread::sleep(Duration::from_millis(3500));
  for f in &fibers {
    f.interrupt_now();
  }
  for f in fibers {
    let _ = db.rt.block_on(f.join_future());
  }

  let rows = db.ticks();
  let keys: BTreeSet<_> = rows.iter().map(|(k, _)| k.clone()).collect();
  assert!(rows.len() >= 3, "a tick per second: {rows:?}");
  assert_eq!(keys.len(), rows.len(), "each tick inserted once: {rows:?}");
  let (by_a, by_b) = (succeeded(&a.scheduler()), succeeded(&b.scheduler()));
  assert!(by_a.is_subset(&keys) && by_b.is_subset(&keys));
  let both = by_a.intersection(&by_b).count();
  assert!(
    both >= 2,
    "both schedulers enqueued the same ticks: {by_a:?} {by_b:?}"
  );

  // The key outlives the job: once the first tick's job succeeded, enqueuing it again (a
  // scheduler that woke up late) still inserts nothing.
  let (first, _) = &rows[0];
  assert!(
    wait_until(Duration::from_secs(10), || db.count(
      "SELECT count(*) FROM id_effect_jobs WHERE unique_key = $1 AND state = 'succeeded'",
      &params![first.clone()],
    ) == 1),
    "the first tick's job ran"
  );
  let late = PgJobQueue::new(db.client.clone()).enqueue(
    Beat { by: "late".into() },
    JobOptions::new().unique(id_effect_jobs::UniqueKey::until_pruned(first.clone())),
  );
  assert!(matches!(
    db.run(late),
    Enqueued::Duplicate { existing: Some(_) }
  ));
  assert_eq!(
    db.count("SELECT count(*) FROM id_effect_jobs", &[]),
    rows.len() as i64
  );
}

#[test]
fn lease_loss_hands_over_schedule() {
  let test = "lease_loss_hands_over_schedule";
  let Some(db) = Db::new(test) else {
    return;
  };
  let lock = format!("id_effect-cron-it-{}", std::process::id());
  let app_a = format!("idfx_cron_a_{}", std::process::id());
  // One advisory-lock session per node; `a`'s is tagged so the test can kill it.
  let lease = |app: &str| {
    let config = PgPoolConfig::from_url(with_param(&db.url, &format!("application_name={app}")));
    db.run(PgAdvisoryLease::from_pool_config(config))
  };
  let leader = |holder: &str, lease: PgAdvisoryLease| {
    let single = ClusterSingleton::new(&lock, holder, Arc::new(lease), Duration::from_secs(30));
    CronLeader::new(
      single,
      Pid::from_parts(NodeName::new(format!("{holder}@it")), 1, 1),
    )
    .retry_every(Duration::from_millis(100))
    .heartbeat_every(Duration::from_millis(100))
  };
  let a = leader("a", lease(&app_a));
  let b = leader("b", lease("idfx_cron_b"));
  let holds = |l: &CronLeader| db.run(l.singleton().is_holder());

  let fa = db
    .rt
    .fork(a.run(&beat("a"), WallClock::system()).provide_all(db.env()));
  assert!(wait_until(Duration::from_secs(10), || holds(&a)), "a leads");
  let fb = db
    .rt
    .fork(b.run(&beat("b"), WallClock::system()).provide_all(db.env()));
  std::thread::sleep(Duration::from_millis(2500));
  assert!(!holds(&b), "b follows while a holds the lock");

  // `a`'s lock session dies (a crashed node, a network cut): Postgres frees the lock, `a`'s
  // heartbeat fails and it stops its schedule, and `b` acquires the lock and takes over.
  let killed = db.count(
    "SELECT count(pg_terminate_backend(pid)) FROM pg_stat_activity WHERE application_name = $1",
    &params![app_a.clone()],
  );
  assert_eq!(killed, 1);
  assert!(
    wait_until(Duration::from_secs(10), || holds(&b)),
    "b takes over"
  );
  assert!(
    wait_until(Duration::from_secs(5), || !holds(&a)),
    "a stepped down"
  );
  std::thread::sleep(Duration::from_millis(2500));
  fa.interrupt_now();
  fb.interrupt_now();
  let _ = db.rt.block_on(fa.join_future());
  let _ = db.rt.block_on(fb.join_future());

  let rows = db.ticks();
  let keys: BTreeSet<_> = rows.iter().map(|(k, _)| k.clone()).collect();
  assert_eq!(keys.len(), rows.len(), "each tick inserted once: {rows:?}");
  let last_a = rows.iter().rposition(|(_, by)| by == "a");
  let first_b = rows.iter().position(|(_, by)| by == "b");
  match (last_a, first_b) {
    (Some(last_a), Some(first_b)) => {
      assert!(last_a < first_b, "a's ticks all precede b's: {rows:?}")
    }
    _ => panic!("both nodes led a part of the schedule: {rows:?}"),
  }
  assert!(
    rows.iter().filter(|(_, by)| by == "b").count() >= 2,
    "b kept the schedule going: {rows:?}"
  );
}
