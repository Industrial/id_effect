//! Durable sagas on PostgreSQL (fault.workflows#3): the compensations a saga registered run as
//! journaled activities, in reverse order, at least once, and the rollback survives a crashed
//! worker, a crashed engine and a restart onto another pool.
//!
//! The tests need a database at `DATABASE_URL`. When CI provides a Postgres service it also sets
//! `ID_EFFECT_REQUIRE_PG=1`, which turns a missing or unreachable database into a test
//! **failure**; without it they print an explicit `SKIP` line to stderr and return. Each test runs
//! in a schema of its own (`search_path`), so parallel test processes never share rows.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use id_effect::{Effect, Exit, Schedule, Schema, UtcDateTime, WallClock, fail, hlist, sync};
use id_effect_jobs::{
  JobFilter, JobQueue, JobQueueEnv, JobQueueService, JobRegistry, JobState, JobStore, PgJobQueue,
  Queue, Worker, WorkerConfig, WorkerId,
};
use id_effect_sql::SqlClient;
use id_effect_sql::pg::{PgPoolConfig, PgSqlClient, pg_pool_from_config};
use id_effect_tokio::TokioRuntime;
use id_effect_workflow::{
  ACTIVITY_QUEUE, Activity, ActivityContext, EngineConfig, EngineId, EventKind, JobFailure,
  PgWorkflowStore, Workflow, WorkflowContext, WorkflowEngine, WorkflowError, WorkflowId,
  WorkflowStatus, WorkflowStore, migrator,
};

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
  /// The test schema's connection string (a restarted engine opens its own pool on it).
  url: String,
  client: PgSqlClient,
  /// The test clock every engine, queue and worker reads.
  clock: WallClock,
}

impl Db {
  /// A client on the fresh schema `saga_<test>` with the workflow and job tables.
  fn new(test: &str) -> Option<Self> {
    let url = match std::env::var("DATABASE_URL") {
      Ok(url) => url,
      Err(_) => return unavailable(test, "DATABASE_URL is not set"),
    };
    let rt = TokioRuntime::new_multi_thread().expect("tokio runtime");
    let schema = format!("saga_{test}");
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
    let client = Self::connect(&rt, &scoped);
    let clock = WallClock::anchored(rt.now(), UtcDateTime::unsafe_make(1_700_000_000_000));
    let db = Self {
      rt,
      url: scoped,
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

  fn connect(rt: &TokioRuntime, url: &str) -> PgSqlClient {
    let pool = rt
      .block_on(pg_pool_from_config(
        PgPoolConfig::from_url(url.to_owned()).with_max_size(16),
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

  fn queue(&self, client: &PgSqlClient) -> PgJobQueue {
    PgJobQueue::new(client.clone()).with_clock(self.clock.clone())
  }

  /// An engine named `name` on `client`, with `Trip` registered, and its workers' registry.
  fn engine(
    &self,
    name: &str,
    client: &PgSqlClient,
    log: &Log,
  ) -> (WorkflowEngine, JobRegistry<JobQueueEnv>) {
    let engine = WorkflowEngine::new(PgWorkflowStore::new(client.clone()))
      .with_config(EngineConfig {
        id: EngineId::new(name),
        lease: Duration::from_secs(30),
        poll_interval: Duration::from_millis(20),
      })
      .with_clock(self.clock.clone())
      .with_job_queue(self.queue(client))
      .register(Trip)
      .unwrap();
    let registry = engine
      .register_activity(JobRegistry::new(), Reserve(Arc::clone(log)))
      .unwrap();
    let registry = engine
      .register_activity(registry, Release(Arc::clone(log)))
      .unwrap();
    let registry = engine.register_activity(registry, Charge).unwrap();
    (engine, registry)
  }

  /// Runs every runnable job of the activity queue once on a worker of `registry`.
  fn drain(&self, client: &PgSqlClient, registry: &JobRegistry<JobQueueEnv>) {
    let config = WorkerConfig {
      id: WorkerId::new("w"),
      clock: self.clock.clone(),
      ..WorkerConfig::default()
    };
    let env = hlist![JobQueueService::new(self.queue(client))];
    let worker = Worker::new(registry.clone())
      .queue(ACTIVITY_QUEUE, 4)
      .with_config(config);
    self.run(worker.run_until_idle().provide_all(env));
  }

  /// Resumes `id` and drains the queue until `done` holds or it finishes (at most 60 s).
  fn advance(
    &self,
    client: &PgSqlClient,
    (engine, registry): &(WorkflowEngine, JobRegistry<JobQueueEnv>),
    id: &WorkflowId,
    done: impl Fn(&Self) -> bool,
  ) -> WorkflowStatus {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
      self.run(engine.resume(id));
      let status = self.run(engine.status(id));
      if done(self) || status.is_terminal() || Instant::now() > deadline {
        return status;
      }
      self.drain(client, registry);
      std::thread::sleep(Duration::from_millis(15));
    }
  }

  fn jobs(&self) -> Vec<id_effect_jobs::JobRecord> {
    self.run(self.queue(&self.client).list(JobFilter::default()))
  }
}

type Log = Arc<Mutex<Vec<String>>>;

fn entry(log: &Log, line: String) -> Effect<(), JobFailure> {
  let log = Arc::clone(log);
  sync(move || log.lock().unwrap().push(line))
}

#[derive(Clone, Debug, PartialEq, Schema)]
struct Item {
  n: i64,
}

/// Reserves item `n`.
struct Reserve(Log);

impl Activity for Reserve {
  type Input = Item;
  type Output = Item;
  const NAME: &'static str = "reserve";

  fn run(&self, _: ActivityContext, item: Item) -> Effect<Item, JobFailure> {
    entry(&self.0, format!("reserve {}", item.n)).map(move |()| item)
  }
}

/// Releases item `n` (the compensation of `reserve`).
struct Release(Log);

impl Activity for Release {
  type Input = Item;
  type Output = Item;
  const NAME: &'static str = "release";

  fn run(&self, _: ActivityContext, item: Item) -> Effect<Item, JobFailure> {
    entry(&self.0, format!("release {}", item.n)).map(move |()| item)
  }
}

/// Always declines; two attempts.
struct Charge;

impl Activity for Charge {
  type Input = Item;
  type Output = Item;
  const NAME: &'static str = "charge";
  const MAX_ATTEMPTS: u16 = 2;

  fn retry() -> Schedule<(), JobFailure> {
    Schedule::spaced(Duration::from_millis(10)).map(|_| ())
  }

  fn run(&self, _: ActivityContext, _: Item) -> Effect<Item, JobFailure> {
    fail(JobFailure::retry("card declined"))
  }
}

/// Reserves items 1 and 2 (each compensated by `release`), then charges.
struct Trip;

impl Workflow for Trip {
  type Input = Item;
  type Output = Item;
  const NAME: &'static str = "trip";

  fn run(&self, ctx: WorkflowContext, total: Item) -> Effect<Item, WorkflowError> {
    let charge = ctx.clone();
    ctx.saga(move |saga| {
      let second = saga.clone();
      saga
        .activity::<Reserve, Release, _>(Item { n: 1 }, Item::clone)
        .flat_map(move |_| second.activity::<Reserve, Release, _>(Item { n: 2 }, Item::clone))
        .flat_map(move |_| charge.activity::<Charge>(total))
    })
  }
}

fn id(s: &str) -> WorkflowId {
  WorkflowId::new(s).unwrap()
}

#[test]
fn saga_compensates_in_reverse_after_restart() {
  let test = "saga_compensates_in_reverse_after_restart";
  let Some(db) = Db::new(test) else {
    return;
  };
  let log = Log::default();
  let first = db.engine("a", &db.client, &log);
  let i = id("trip-1");
  db.run(first.0.start::<Trip>(i.clone(), Item { n: 9 }));

  // Both reservations, the declined charge, and the first compensation scheduled: `release`
  // of item 2, registered last.
  let rolling_back = |db: &Db| db.jobs().iter().any(|job| job.name == "release");
  assert_eq!(
    db.advance(&db.client, &first, &i, rolling_back),
    WorkflowStatus::Running
  );
  assert_eq!(*log.lock().unwrap(), ["reserve 1", "reserve 2"]);
  let parked = db.run(first.0.instance(&i)).unwrap();
  assert!(parked.waiting, "waiting for the compensation");
  drop(first);

  // A worker takes the compensation, runs it, and dies before recording anything.
  let queue = db.queue(&db.client);
  let claimed = db.run(queue.claim(
    &Queue::new(ACTIVITY_QUEUE),
    "crashed",
    1,
    Duration::from_secs(5),
  ));
  assert_eq!(claimed.len(), 1);
  assert_eq!(claimed[0].name, "release");
  let ctx = ActivityContext {
    instance: i.clone(),
    seq: 6,
    attempt: 1,
    max_attempts: 20,
  };
  db.run(Release(Arc::clone(&log)).run(ctx, Item { n: 2 }));

  // Restart: a new pool, engine and workers. The rescued job runs the compensation again.
  let client = Db::connect(&db.rt, &db.url);
  let second = db.engine("b", &client, &log);
  db.clock.jump(Duration::from_secs(6));
  assert_eq!(db.run(db.queue(&client).rescue_expired()), 1);
  db.drain(&client, &second.1);

  // An engine takes the woken instance and crashes holding its lease; `b` waits it out.
  let store = PgWorkflowStore::new(client.clone());
  let now = db.run(db.clock.now::<WorkflowError, id_effect::HNil>());
  let until = UtcDateTime(now.0 + jiff::SignedDuration::from_secs(30));
  let crashed = db.run(store.acquire(&i, &EngineId::new("crashed"), now, until));
  assert!(crashed.is_some(), "the delivery woke the instance");
  assert_eq!(
    db.run(second.0.resume(&i)),
    None,
    "leased to the crashed engine"
  );
  db.clock.jump(Duration::from_secs(31));

  assert_eq!(
    db.advance(&client, &second, &i, |_| false),
    WorkflowStatus::Failed
  );
  assert_eq!(
    *log.lock().unwrap(),
    [
      "reserve 1",
      "reserve 2",
      "release 2",
      "release 2",
      "release 1"
    ],
    "reverse order, at least once"
  );
  let history = db.run(store.history(&i));
  let kinds: Vec<(EventKind, &str)> = history.iter().map(|e| (e.kind, e.name.as_str())).collect();
  assert_eq!(
    kinds,
    [
      (EventKind::Activity, "reserve"),
      (EventKind::Step, "compensate:release"),
      (EventKind::Activity, "reserve"),
      (EventKind::Step, "compensate:release"),
      (EventKind::Activity, "charge"),
      (EventKind::Activity, "release"),
      (EventKind::Activity, "release"),
    ]
  );
  assert_eq!(history[5].payload, serde_json::json!({ "ok": { "n": 2 } }));
  assert_eq!(history[6].payload, serde_json::json!({ "ok": { "n": 1 } }));
  let release = |seq: i64| {
    db.jobs()
      .into_iter()
      .find(|job| job.unique.as_ref().map(|u| u.key.clone()) == Some(format!("trip-1#{seq}")))
      .expect("the compensation's job")
  };
  assert_eq!(
    (release(6).state, release(6).attempts),
    (JobState::Succeeded, 2)
  );
  assert_eq!(release(7).state, JobState::Succeeded);
  match db.rt.block_on(db.rt.run(second.0.result::<Trip>(&i))) {
    Exit::Failure(id_effect::Cause::Fail(WorkflowError::Failed(message))) => {
      assert_eq!(message, "activity `charge` failed: card declined");
    }
    other => panic!("expected the recorded failure, got {other:?}"),
  }
}

#[test]
fn completed_saga_runs_no_compensation() {
  let test = "completed_saga_runs_no_compensation";
  let Some(db) = Db::new(test) else {
    return;
  };
  struct Paid;
  impl Workflow for Paid {
    type Input = Item;
    type Output = Item;
    const NAME: &'static str = "paid";

    fn run(&self, ctx: WorkflowContext, item: Item) -> Effect<Item, WorkflowError> {
      ctx.saga(move |saga| saga.activity::<Reserve, Release, _>(item, Item::clone))
    }
  }
  let log = Log::default();
  let (engine, registry) = db.engine("a", &db.client, &log);
  let engine = engine.register(Paid).unwrap();
  let pair = (engine, registry);
  let i = id("paid-1");
  db.run(pair.0.start::<Paid>(i.clone(), Item { n: 3 }));
  assert_eq!(
    db.advance(&db.client, &pair, &i, |_| false),
    WorkflowStatus::Completed
  );
  assert_eq!(db.run(pair.0.result::<Paid>(&i)), Some(Item { n: 3 }));
  assert_eq!(*log.lock().unwrap(), ["reserve 3"]);
  assert_eq!(db.jobs().len(), 1, "no compensation was scheduled");
  assert_eq!(db.run(pair.0.resume(&i)), None, "finished");
}
