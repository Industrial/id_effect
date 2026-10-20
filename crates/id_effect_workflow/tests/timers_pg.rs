//! Durable timers on PostgreSQL (fault.workflows#4): `ctx.sleep` and `ctx.sleep_until` park the
//! instance and fire through the job queue's `run_at` (one job per timer, unique key), a timer
//! survives an engine restart, and waits of days complete under a test clock.
//!
//! The tests need a database at `DATABASE_URL`. When CI provides a Postgres service it also sets
//! `ID_EFFECT_REQUIRE_PG=1`, which turns a missing or unreachable database into a test
//! **failure**; without it they print an explicit `SKIP` line to stderr and return. Each test runs
//! in a schema of its own (`search_path`), so parallel test processes never share rows.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use id_effect::{Effect, Exit, Schema, UtcDateTime, WallClock, hlist, sync};
use id_effect_jobs::{
  DrainReport, JobFilter, JobQueue, JobQueueEnv, JobQueueService, JobRegistry, JobState,
  PgJobQueue, Worker, WorkerConfig, WorkerId,
};
use id_effect_sql::SqlClient;
use id_effect_sql::pg::{PgPoolConfig, PgSqlClient, pg_pool_from_config};
use id_effect_tokio::TokioRuntime;
use id_effect_workflow::{
  EngineConfig, EngineId, EventKind, PgWorkflowStore, RunOutcome, TIMER_QUEUE, Workflow,
  WorkflowContext, WorkflowEngine, WorkflowError, WorkflowId, WorkflowStatus, migrator,
};

const DAY: Duration = Duration::from_secs(24 * 3600);

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
  /// `DATABASE_URL` scoped to the test's schema.
  scoped: String,
  client: PgSqlClient,
  /// The test clock every engine, queue and worker reads.
  clock: WallClock,
}

/// Where the test clock starts.
const START: i64 = 1_700_000_000_000;

impl Db {
  /// A client on the fresh schema `tmr_<test>` with the workflow and job tables.
  fn new(test: &str) -> Option<Self> {
    let url = match std::env::var("DATABASE_URL") {
      Ok(url) => url,
      Err(_) => return unavailable(test, "DATABASE_URL is not set"),
    };
    let rt = TokioRuntime::new_multi_thread().expect("tokio runtime");
    let schema = format!("tmr_{test}");
    let admin = match rt.block_on(pg_pool_from_config(
      PgPoolConfig::from_url(url.clone()).with_max_size(1),
    )) {
      Ok(pool) => PgSqlClient::new(pool),
      Err(e) => return unavailable(test, &format!("cannot connect: {}", e.0)),
    };
    let create = format!("CREATE SCHEMA {schema}");
    let fresh = admin
      .execute(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE"), &[])
      .flat_map(move |_| admin.execute(&create, &[]));
    if let Exit::Failure(c) = rt.block_on(rt.run(fresh)) {
      return unavailable(test, &format!("{c:?}"));
    }
    let sep = if url.contains('?') { '&' } else { '?' };
    let scoped = format!("{url}{sep}options=-c%20search_path%3D{schema}");
    let client = Self::pool(&rt, &scoped);
    let clock = WallClock::anchored(rt.now(), UtcDateTime::unsafe_make(START));
    let db = Self {
      rt,
      scoped,
      client,
      clock,
    };
    let client = Arc::new(db.client.clone());
    let applied = db.run(migrator().run_on(client.clone()).map_err(|e| e.to_string()));
    assert_eq!(applied, [1, 2]);
    let jobs = id_effect_jobs::migrator().run_on(client);
    assert_eq!(db.run(jobs.map_err(|e| e.to_string())), [1]);
    Some(db)
  }

  fn pool(rt: &TokioRuntime, scoped: &str) -> PgSqlClient {
    let pool = rt
      .block_on(pg_pool_from_config(
        PgPoolConfig::from_url(scoped.to_owned()).with_max_size(8),
      ))
      .expect("pool on the test schema");
    PgSqlClient::new(pool)
  }

  fn run<A: Send + 'static, E: std::fmt::Debug + Send + 'static>(&self, eff: Effect<A, E>) -> A {
    match self.rt.block_on(self.rt.run(eff)) {
      Exit::Success(a) => a,
      Exit::Failure(c) => panic!("failed: {c:?}"),
    }
  }

  /// An engine and a worker registry on a pool of their own (one "process").
  fn process(
    &self,
    name: &str,
    wakes: &Arc<AtomicI64>,
  ) -> (WorkflowEngine, PgJobQueue, JobRegistry<JobQueueEnv>) {
    let client = Self::pool(&self.rt, &self.scoped);
    let queue = PgJobQueue::new(client.clone()).with_clock(self.clock.clone());
    let engine = WorkflowEngine::new(PgWorkflowStore::new(client))
      .with_config(EngineConfig {
        id: EngineId::new(name),
        lease: Duration::from_secs(30),
        poll_interval: Duration::from_millis(20),
      })
      .with_clock(self.clock.clone())
      .with_job_queue(queue.clone())
      .register(Reminder {
        wakes: Arc::clone(wakes),
      })
      .unwrap();
    let registry = engine.register_timers(JobRegistry::new()).unwrap();
    (engine, queue, registry)
  }

  /// Runs every due timer job once.
  fn drain(&self, queue: &PgJobQueue, registry: &JobRegistry<JobQueueEnv>) -> DrainReport {
    let config = WorkerConfig {
      id: WorkerId::new("timers"),
      clock: self.clock.clone(),
      ..WorkerConfig::default()
    };
    let env = hlist![JobQueueService::new(queue.clone())];
    let worker = Worker::new(registry.clone())
      .queue(TIMER_QUEUE, 4)
      .with_config(config);
    self.run(worker.run_until_idle().provide_all(env))
  }
}

fn id(s: &str) -> WorkflowId {
  WorkflowId::new(s).unwrap()
}

fn at(ms: i64) -> UtcDateTime {
  UtcDateTime::unsafe_make(ms)
}

#[derive(Clone, Debug, PartialEq, Schema)]
struct Plan {
  /// The first wait, in seconds.
  wait: i64,
  /// Then wait until this instant (ms since the epoch), when not 0.
  until: i64,
}

#[derive(Clone, Debug, PartialEq, Schema)]
struct Woken {
  count: i64,
}

/// Sleeps `wait` seconds, then until `until`, then journals a step counting its runs.
struct Reminder {
  wakes: Arc<AtomicI64>,
}

impl Workflow for Reminder {
  type Input = Plan;
  type Output = Woken;
  const NAME: &'static str = "reminder";

  fn run(&self, ctx: WorkflowContext, plan: Plan) -> Effect<Woken, WorkflowError> {
    let wakes = Arc::clone(&self.wakes);
    let (c2, c3) = (ctx.clone(), ctx.clone());
    let wait = Duration::from_secs(plan.wait.unsigned_abs());
    ctx
      .sleep(wait)
      .flat_map(move |()| {
        if plan.until == 0 {
          id_effect::succeed(())
        } else {
          c2.sleep_until(at(plan.until))
        }
      })
      .flat_map(move |()| {
        let woke: Effect<Woken, String> = sync(move || Woken {
          count: wakes.fetch_add(1, Ordering::SeqCst) + 1,
        });
        c3.step("remind", woke)
      })
  }
}

#[test]
fn durable_timer_survives_engine_restart() {
  let test = "durable_timer_survives_engine_restart";
  let Some(db) = Db::new(test) else {
    return;
  };
  let wakes = Arc::new(AtomicI64::new(0));
  let (a, queue_a, _) = db.process("a", &wakes);
  let i = id("remind-1");
  db.run(a.start::<Reminder>(i.clone(), Plan { wait: 60, until: 0 }));
  let before = db.clock.at(db.rt.now());
  assert_eq!(db.run(a.resume(&i)), Some(RunOutcome::Waiting));
  let after = db.clock.at(db.rt.now());

  // One timer job, due 60 s after the first run, under the key `remind-1#1`.
  let jobs = db.run(queue_a.list(JobFilter::default()));
  assert_eq!(jobs.len(), 1);
  let job = &jobs[0];
  assert_eq!(
    (job.queue.as_str(), job.state),
    (TIMER_QUEUE, JobState::Scheduled)
  );
  assert_eq!(
    job.unique.as_ref().map(|u| u.key.as_str()),
    Some("remind-1#1")
  );
  let due = job.run_at;
  // `before` and `after` bracket the first run's clock reading (the due is whole microseconds).
  let minute = jiff::SignedDuration::from_secs(60);
  let slack = jiff::SignedDuration::from_micros(1);
  assert!(
    due.0 >= before.0 + minute - slack && due.0 <= after.0 + minute,
    "{before:?} {due:?} {after:?}"
  );
  drop((a, queue_a));

  // The engine "restarts": a new process (its own pool) finds the instance parked and the
  // timer not yet due.
  let (b, queue_b, timers) = db.process("b", &wakes);
  assert_eq!(db.run(b.resume(&i)), None, "parked");
  assert_eq!(db.drain(&queue_b, &timers).runs(), 0, "not due yet");
  db.clock.jump(Duration::from_secs(61));
  assert_eq!(db.drain(&queue_b, &timers).succeeded, 1);
  assert_eq!(db.run(b.resume(&i)), Some(RunOutcome::Completed));

  assert_eq!(db.run(b.result::<Reminder>(&i)), Some(Woken { count: 1 }));
  let history = db.run(b.store().history(&i));
  let kinds: Vec<(EventKind, &str)> = history.iter().map(|e| (e.kind, e.name.as_str())).collect();
  assert_eq!(
    kinds,
    [(EventKind::Timer, "timer"), (EventKind::Step, "remind")]
  );
  assert_eq!(
    history[0].payload,
    serde_json::json!({ "due": due.0.to_string() })
  );
  let jobs = db.run(queue_b.list(JobFilter::default()));
  assert_eq!((jobs.len(), jobs[0].state), (1, JobState::Succeeded));
}

#[test]
fn timer_replay_before_it_fires_keeps_the_first_due() {
  let test = "timer_replay_before_it_fires_keeps_the_first_due";
  let Some(db) = Db::new(test) else {
    return;
  };
  let wakes = Arc::new(AtomicI64::new(0));
  let (engine, queue, timers) = db.process("e", &wakes);
  let i = id("remind-replay");
  db.run(engine.start::<Reminder>(
    i.clone(),
    Plan {
      wait: 600,
      until: 0,
    },
  ));
  assert_eq!(db.run(engine.resume(&i)), Some(RunOutcome::Waiting));
  let first = db.run(queue.list(JobFilter::default()))[0].run_at;

  // Woken early (an unrelated signal), 5 minutes later: the replay schedules seq 1 again,
  // which inserts nothing, so the timer still fires 10 minutes after the first run.
  db.clock.jump(Duration::from_secs(300));
  assert!(db.run(engine.client().signal(&i, "noise", Woken { count: 0 })));
  assert_eq!(db.run(engine.resume(&i)), Some(RunOutcome::Waiting));
  let jobs = db.run(queue.list(JobFilter::default()));
  assert_eq!((jobs.len(), jobs[0].run_at), (1, first));
  assert!(db.run(engine.store().history(&i)).is_empty());
  db.clock.jump(Duration::from_secs(301));
  assert_eq!(db.drain(&queue, &timers).succeeded, 1);
  assert_eq!(db.run(engine.resume(&i)), Some(RunOutcome::Completed));
  let history = db.run(engine.store().history(&i));
  assert_eq!(
    history[0].payload,
    serde_json::json!({ "due": first.0.to_string() })
  );
}

#[test]
fn parked_timers_cost_no_engine_runs() {
  let test = "parked_timers_cost_no_engine_runs";
  let Some(db) = Db::new(test) else {
    return;
  };
  let wakes = Arc::new(AtomicI64::new(0));
  let (engine, queue, timers) = db.process("e", &wakes);
  let ids: Vec<WorkflowId> = (0..5).map(|n| id(&format!("park-{n}"))).collect();
  for i in &ids {
    db.run(engine.start::<Reminder>(
      i.clone(),
      Plan {
        wait: 3600,
        until: 0,
      },
    ));
  }
  assert_eq!(db.run(engine.run_once()), 5, "each runs once and parks");
  assert_eq!(
    db.run(engine.run_once()),
    0,
    "parked instances are not claimed"
  );
  for i in &ids {
    let record = db.run(engine.instance(i)).unwrap();
    assert!(record.waiting && record.lease_owner.is_none(), "{record:?}");
  }
  db.clock.jump(Duration::from_secs(3601));
  assert_eq!(db.drain(&queue, &timers).succeeded, 5);
  assert_eq!(db.run(engine.run_once()), 5);
  assert_eq!(wakes.load(Ordering::SeqCst), 5);
  for i in &ids {
    assert_eq!(db.run(engine.status(i)), WorkflowStatus::Completed);
  }
}

#[test]
fn timer_of_days_under_test_clock() {
  let test = "timer_of_days_under_test_clock";
  let Some(db) = Db::new(test) else {
    return;
  };
  let wakes = Arc::new(AtomicI64::new(0));
  let (engine, queue, timers) = db.process("e", &wakes);
  let i = id("remind-days");
  let five_days = START + 5 * 86_400_000;
  let plan = Plan {
    wait: 3 * 86_400,
    until: five_days,
  };
  db.run(engine.start::<Reminder>(i.clone(), plan));
  assert_eq!(db.run(engine.resume(&i)), Some(RunOutcome::Waiting));
  assert_eq!(db.drain(&queue, &timers).runs(), 0);

  // Three days pass in an instant: the first timer fires, the second one is scheduled.
  db.clock.jump(3 * DAY);
  assert_eq!(db.drain(&queue, &timers).succeeded, 1);
  assert_eq!(db.run(engine.resume(&i)), Some(RunOutcome::Waiting));
  let mut jobs = db.run(queue.list(JobFilter::default()));
  jobs.sort_by_key(|j| j.id);
  assert_eq!(jobs.len(), 2);
  assert_eq!(jobs[1].run_at, at(five_days));
  assert_eq!(
    jobs[1].unique.as_ref().map(|u| u.key.as_str()),
    Some("remind-days#2")
  );
  assert_eq!(db.drain(&queue, &timers).runs(), 0, "day 5 has not come");

  db.clock.jump(2 * DAY);
  assert_eq!(db.drain(&queue, &timers).succeeded, 1);
  assert_eq!(db.run(engine.resume(&i)), Some(RunOutcome::Completed));
  assert_eq!(db.run(engine.status(&i)), WorkflowStatus::Completed);
  assert_eq!(wakes.load(Ordering::SeqCst), 1, "the step ran once");
  let history = db.run(engine.store().history(&i));
  assert_eq!(history.len(), 3);
  assert_eq!(
    history[1].payload,
    serde_json::json!({ "due": at(five_days).0.to_string() })
  );

  // A sleep_until in the past fires at once.
  let j = id("remind-past");
  let past = Plan {
    wait: 0,
    until: START,
  };
  db.run(engine.start::<Reminder>(j.clone(), past));
  for _ in 0..2 {
    assert_eq!(db.run(engine.resume(&j)), Some(RunOutcome::Waiting));
    assert_eq!(db.drain(&queue, &timers).succeeded, 1);
  }
  assert_eq!(db.run(engine.resume(&j)), Some(RunOutcome::Completed));
}
