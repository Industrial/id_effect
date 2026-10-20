//! Activities on PostgreSQL (fault.workflows#7): an activity runs as an `id_effect_jobs` job,
//! is retried by its `Schedule` until it succeeds, runs again when its worker crashed, and is
//! scheduled and delivered once per idempotency key `(instance, seq)`; its result is journaled.
//!
//! The tests need a database at `DATABASE_URL`. When CI provides a Postgres service it also sets
//! `ID_EFFECT_REQUIRE_PG=1`, which turns a missing or unreachable database into a test
//! **failure**; without it they print an explicit `SKIP` line to stderr and return. Each test runs
//! in a schema of its own (`search_path`), so parallel test processes never share rows.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, Instant};

use id_effect::{
  Effect, Exit, Schedule, Schema, UtcDateTime, WallClock, fail, hlist, succeed, sync,
};
use id_effect_jobs::{
  DrainReport, JobFilter, JobQueue, JobQueueEnv, JobQueueService, JobRegistry, JobState, JobStore,
  PgJobQueue, Queue, Worker, WorkerConfig, WorkerId,
};
use id_effect_sql::SqlClient;
use id_effect_sql::pg::{PgPoolConfig, PgSqlClient, pg_pool_from_config};
use id_effect_tokio::TokioRuntime;
use id_effect_workflow::{
  ACTIVITY_QUEUE, Activity, ActivityContext, Completion, EngineConfig, EngineId, EventKind,
  JobFailure, PgWorkflowStore, RunOutcome, Workflow, WorkflowContext, WorkflowEngine,
  WorkflowError, WorkflowId, WorkflowStatus, WorkflowStore, migrator,
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
  client: PgSqlClient,
  /// The test clock every engine, queue and worker reads.
  clock: WallClock,
}

impl Db {
  /// A client on the fresh schema `act_<test>` with the workflow and job tables.
  fn new(test: &str) -> Option<Self> {
    let url = match std::env::var("DATABASE_URL") {
      Ok(url) => url,
      Err(_) => return unavailable(test, "DATABASE_URL is not set"),
    };
    let rt = TokioRuntime::new_multi_thread().expect("tokio runtime");
    let schema = format!("act_{test}");
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
    let pool = rt
      .block_on(pg_pool_from_config(
        PgPoolConfig::from_url(scoped).with_max_size(16),
      ))
      .expect("pool on the test schema");
    let clock = WallClock::anchored(rt.now(), UtcDateTime::unsafe_make(1_700_000_000_000));
    let db = Self {
      rt,
      client: PgSqlClient::new(pool),
      clock,
    };
    let client = Arc::new(db.client.clone());
    let applied = db.run(migrator().run_on(client.clone()).map_err(|e| e.to_string()));
    assert_eq!(applied, [1, 2]);
    let jobs = id_effect_jobs::migrator().run_on(client);
    assert_eq!(db.run(jobs.map_err(|e| e.to_string())), [1]);
    Some(db)
  }

  fn run<A: Send + 'static, E: std::fmt::Debug + Send + 'static>(&self, eff: Effect<A, E>) -> A {
    match self.rt.block_on(self.rt.run(eff)) {
      Exit::Success(a) => a,
      Exit::Failure(c) => panic!("failed: {c:?}"),
    }
  }

  fn store(&self) -> PgWorkflowStore {
    PgWorkflowStore::new(self.client.clone())
  }

  fn queue(&self) -> PgJobQueue {
    PgJobQueue::new(self.client.clone()).with_clock(self.clock.clone())
  }

  fn engine(&self, name: &str) -> WorkflowEngine {
    WorkflowEngine::new(self.store())
      .with_config(EngineConfig {
        id: EngineId::new(name),
        lease: Duration::from_secs(30),
        poll_interval: Duration::from_millis(20),
      })
      .with_clock(self.clock.clone())
      .with_job_queue(self.queue())
  }

  /// Runs every runnable job of the activity queue once on a worker named `worker`.
  fn drain(&self, registry: &JobRegistry<JobQueueEnv>, worker: &str) -> DrainReport {
    let config = WorkerConfig {
      id: WorkerId::new(worker),
      clock: self.clock.clone(),
      ..WorkerConfig::default()
    };
    let env = hlist![JobQueueService::new(self.queue())];
    let worker = Worker::new(registry.clone())
      .queue(ACTIVITY_QUEUE, 4)
      .with_config(config);
    self.run(worker.run_until_idle().provide_all(env))
  }

  /// Resumes `id` until it finishes, draining the queue in between (at most 60 s).
  fn finish(
    &self,
    engine: &WorkflowEngine,
    registry: &JobRegistry<JobQueueEnv>,
    id: &WorkflowId,
  ) -> WorkflowStatus {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
      self.run(engine.resume(id));
      let status = self.run(engine.status(id));
      if status.is_terminal() || Instant::now() > deadline {
        return status;
      }
      self.drain(registry, "w");
      std::thread::sleep(Duration::from_millis(15));
    }
  }

  fn jobs(&self) -> Vec<id_effect_jobs::JobRecord> {
    self.run(self.queue().list(JobFilter::default()))
  }
}

fn id(s: &str) -> WorkflowId {
  WorkflowId::new(s).unwrap()
}

#[derive(Clone, Debug, PartialEq, Schema)]
struct Order {
  id: i64,
}

#[derive(Clone, Debug, PartialEq, Schema)]
struct Receipt {
  order: i64,
  charged: i64,
}

/// Charges 42; its first `fail_first` attempts fail. Counts every attempt in `runs`.
struct Charge {
  runs: Arc<AtomicI64>,
  fail_first: i64,
}

impl Activity for Charge {
  type Input = Order;
  type Output = Receipt;
  const NAME: &'static str = "charge";

  fn retry() -> Schedule<(), JobFailure> {
    Schedule::spaced(Duration::from_millis(10)).map(|_| ())
  }

  fn run(&self, ctx: ActivityContext, order: Order) -> Effect<Receipt, JobFailure> {
    let (runs, fail_first) = (Arc::clone(&self.runs), self.fail_first);
    assert_eq!(
      ctx.idempotency_key(),
      format!("{}#{}", ctx.instance, ctx.seq)
    );
    sync(move || runs.fetch_add(1, Ordering::SeqCst) + 1).flat_map(move |n| {
      if n <= fail_first {
        fail(JobFailure::retry(format!("attempt {n} declined")))
      } else {
        succeed(Receipt {
          order: order.id,
          charged: 42,
        })
      }
    })
  }
}

/// Always declines; two attempts.
struct Decline;

impl Activity for Decline {
  type Input = Order;
  type Output = Receipt;
  const NAME: &'static str = "decline";
  const MAX_ATTEMPTS: u16 = 2;

  fn retry() -> Schedule<(), JobFailure> {
    Schedule::spaced(Duration::from_millis(10)).map(|_| ())
  }

  fn run(&self, _: ActivityContext, _: Order) -> Effect<Receipt, JobFailure> {
    fail(JobFailure::retry("card declined"))
  }
}

/// Charges through the `charge` activity, then journals a step on the receipt.
struct Checkout;

impl Workflow for Checkout {
  type Input = Order;
  type Output = Receipt;
  const NAME: &'static str = "checkout";

  fn run(&self, ctx: WorkflowContext, order: Order) -> Effect<Receipt, WorkflowError> {
    let next = ctx.clone();
    ctx
      .activity::<Charge>(order)
      .flat_map(move |receipt| next.step("confirm", succeed::<_, String, _>(receipt)))
  }
}

/// Runs the `decline` activity.
struct Declined;

impl Workflow for Declined {
  type Input = Order;
  type Output = Receipt;
  const NAME: &'static str = "declined";

  fn run(&self, ctx: WorkflowContext, order: Order) -> Effect<Receipt, WorkflowError> {
    ctx.activity::<Decline>(order)
  }
}

fn registry(
  engine: &WorkflowEngine,
  runs: &Arc<AtomicI64>,
  fail_first: i64,
) -> JobRegistry<JobQueueEnv> {
  let charge = Charge {
    runs: Arc::clone(runs),
    fail_first,
  };
  let registry = engine
    .register_activity(JobRegistry::new(), charge)
    .unwrap();
  engine.register_activity(registry, Decline).unwrap()
}

#[test]
fn activity_retried_until_success_then_journaled() {
  let test = "activity_retried_until_success_then_journaled";
  let Some(db) = Db::new(test) else {
    return;
  };
  let engine = db.engine("e").register(Checkout).unwrap();
  let runs = Arc::new(AtomicI64::new(0));
  let registry = registry(&engine, &runs, 2);
  let i = id("checkout-1");
  db.run(engine.start::<Checkout>(i.clone(), Order { id: 7 }));

  // The first run schedules the activity and parks.
  assert_eq!(db.run(engine.resume(&i)), Some(RunOutcome::Waiting));
  let parked = db.run(engine.instance(&i)).unwrap();
  assert!(parked.waiting && parked.lease_owner.is_none());
  assert_eq!(
    db.run(engine.resume(&i)),
    None,
    "a parked instance is not claimable"
  );
  let jobs = db.jobs();
  assert_eq!(jobs.len(), 1);
  assert_eq!(
    (jobs[0].name.as_str(), jobs[0].queue.as_str()),
    ("charge", ACTIVITY_QUEUE)
  );
  assert_eq!(
    jobs[0].unique.as_ref().map(|u| u.key.as_str()),
    Some("checkout-1#1")
  );

  // Two declined attempts, retried after the 10 ms schedule, then a success.
  assert_eq!(db.finish(&engine, &registry, &i), WorkflowStatus::Completed);
  assert_eq!(runs.load(Ordering::SeqCst), 3);
  assert_eq!(
    db.run(engine.result::<Checkout>(&i)),
    Some(Receipt {
      order: 7,
      charged: 42
    })
  );
  let job = &db.jobs()[0];
  assert_eq!((job.state, job.attempts), (JobState::Succeeded, 3));
  assert_eq!(job.last_error.as_deref(), Some("attempt 2 declined"));
  let history = db.run(db.store().history(&i));
  let kinds: Vec<(EventKind, &str)> = history.iter().map(|e| (e.kind, e.name.as_str())).collect();
  assert_eq!(
    kinds,
    [
      (EventKind::Activity, "charge"),
      (EventKind::Step, "confirm")
    ]
  );
  assert_eq!(
    history[0].payload,
    serde_json::json!({ "ok": { "order": 7, "charged": 42 } })
  );
  // A replay of the finished history runs nothing again.
  assert_eq!(db.drain(&registry, "w").runs(), 0);
}

#[test]
fn activity_final_failure_is_journaled() {
  let test = "activity_final_failure_is_journaled";
  let Some(db) = Db::new(test) else {
    return;
  };
  let engine = db.engine("e").register(Declined).unwrap();
  let registry = registry(&engine, &Arc::new(AtomicI64::new(0)), 0);
  let i = id("declined-1");
  db.run(engine.start::<Declined>(i.clone(), Order { id: 8 }));
  assert_eq!(db.finish(&engine, &registry, &i), WorkflowStatus::Failed);
  let job = &db.jobs()[0];
  assert_eq!((job.state, job.attempts), (JobState::Dead, 2));
  let history = db.run(db.store().history(&i));
  assert_eq!(history.len(), 1);
  assert_eq!(
    history[0].payload,
    serde_json::json!({ "err": "card declined" })
  );
  match db.rt.block_on(db.rt.run(engine.result::<Declined>(&i))) {
    Exit::Failure(id_effect::Cause::Fail(WorkflowError::Failed(message))) => {
      assert!(
        message.contains("activity `decline` failed: card declined"),
        "{message}"
      );
    }
    other => panic!("expected the recorded failure, got {other:?}"),
  }
}

#[test]
fn activity_runs_at_least_once_after_worker_crash() {
  let test = "activity_runs_at_least_once_after_worker_crash";
  let Some(db) = Db::new(test) else {
    return;
  };
  let engine = db.engine("e").register(Checkout).unwrap();
  let runs = Arc::new(AtomicI64::new(0));
  let registry = registry(&engine, &runs, 0);
  let i = id("crash-1");
  db.run(engine.start::<Checkout>(i.clone(), Order { id: 9 }));
  assert_eq!(db.run(engine.resume(&i)), Some(RunOutcome::Waiting));

  // A worker claims the job with a 5 s lease, runs the activity, and dies before recording
  // anything.
  let queue = db.queue();
  let claimed = db.run(queue.claim(
    &Queue::new(ACTIVITY_QUEUE),
    "crashed",
    1,
    Duration::from_secs(5),
  ));
  assert_eq!(claimed.len(), 1);
  let ctx = ActivityContext {
    instance: i.clone(),
    seq: 1,
    attempt: 1,
    max_attempts: 20,
  };
  let lost = Charge {
    runs: Arc::clone(&runs),
    fail_first: 0,
  };
  db.run(lost.run(ctx, Order { id: 9 }));
  assert_eq!(
    db.drain(&registry, "w").runs(),
    0,
    "the job is leased to the dead worker"
  );
  assert_eq!(db.run(engine.resume(&i)), None, "still parked");

  // Its lease runs out; any worker rescues the job and runs it again.
  db.clock.jump(Duration::from_secs(6));
  assert_eq!(db.run(queue.rescue_expired()), 1);
  assert_eq!(db.drain(&registry, "w").succeeded, 1);
  assert_eq!(db.finish(&engine, &registry, &i), WorkflowStatus::Completed);

  assert_eq!(runs.load(Ordering::SeqCst), 2, "at least once: ran twice");
  let job = &db.jobs()[0];
  assert_eq!((job.state, job.attempts), (JobState::Succeeded, 2));
  let history = db.run(db.store().history(&i));
  assert_eq!(history.len(), 2, "delivered once");
  assert_eq!(
    db.run(engine.result::<Checkout>(&i)),
    Some(Receipt {
      order: 9,
      charged: 42
    })
  );
}

#[test]
fn activity_result_dedup_by_idempotency_key() {
  let test = "activity_result_dedup_by_idempotency_key";
  let Some(db) = Db::new(test) else {
    return;
  };
  let a = db.engine("a").register(Checkout).unwrap();
  let runs = Arc::new(AtomicI64::new(0));
  let registry = registry(&a, &runs, 0);
  let i = id("dedup-1");
  db.run(a.start::<Checkout>(i.clone(), Order { id: 10 }));
  assert_eq!(db.run(a.resume(&i)), Some(RunOutcome::Waiting));

  // Woken by an unrelated signal, the instance replays and schedules seq 1 again, on this
  // engine and on another one (its own pool and queue): the key `dedup-1#1` inserts nothing.
  let b = WorkflowEngine::new(PgWorkflowStore::new(db.client.clone()))
    .with_config(EngineConfig {
      id: EngineId::new("b"),
      ..EngineConfig::default()
    })
    .with_clock(db.clock.clone())
    .with_job_queue(db.queue())
    .register(Checkout)
    .unwrap();
  for engine in [&a, &b] {
    assert!(db.run(engine.client().signal(&i, "noise", Order { id: 0 })));
    assert_eq!(db.run(engine.resume(&i)), Some(RunOutcome::Waiting));
  }
  let jobs = db.jobs();
  assert_eq!(jobs.len(), 1, "one call, one job: {jobs:?}");

  // One worker run delivers the result; a second delivery for the key is ignored.
  assert_eq!(db.drain(&registry, "w").succeeded, 1);
  assert_eq!(runs.load(Ordering::SeqCst), 1);
  let delivered = db.run(db.store().completion(&i, 1)).unwrap();
  assert_eq!(
    (delivered.kind, delivered.name.as_str()),
    (EventKind::Activity, "charge")
  );
  let duplicate = Completion {
    payload: serde_json::json!({ "ok": { "order": 10, "charged": 99 } }),
    ..delivered.clone()
  };
  assert!(!db.run(db.store().complete(&i, duplicate)));
  assert_eq!(db.run(db.store().completion(&i, 1)), Some(delivered));

  assert_eq!(db.finish(&a, &registry, &i), WorkflowStatus::Completed);
  assert_eq!(
    db.run(a.result::<Checkout>(&i)),
    Some(Receipt {
      order: 10,
      charged: 42
    }),
    "the first result wins"
  );
  assert_eq!(runs.load(Ordering::SeqCst), 1, "the activity ran once");
  assert_eq!(db.jobs().len(), 1);
}

/// Refunds on its own queue `payments`.
struct Refund;

impl Activity for Refund {
  type Input = Order;
  type Output = Receipt;
  const NAME: &'static str = "refund";
  const QUEUE: &'static str = "payments";

  fn run(&self, _: ActivityContext, order: Order) -> Effect<Receipt, JobFailure> {
    succeed(Receipt {
      order: order.id,
      charged: -42,
    })
  }
}

/// Charges, then refunds: two activity calls at seq 1 and 2.
struct ChargeAndRefund;

impl Workflow for ChargeAndRefund {
  type Input = Order;
  type Output = Receipt;
  const NAME: &'static str = "charge-and-refund";

  fn run(&self, ctx: WorkflowContext, order: Order) -> Effect<Receipt, WorkflowError> {
    let next = ctx.clone();
    ctx
      .activity::<Charge>(order.clone())
      .flat_map(move |_| next.activity::<Refund>(order))
  }
}

#[test]
fn activity_runs_on_its_named_queue_with_one_key_per_call() {
  let test = "activity_runs_on_its_named_queue_with_one_key_per_call";
  let Some(db) = Db::new(test) else {
    return;
  };
  let engine = db.engine("e").register(ChargeAndRefund).unwrap();
  let runs = Arc::new(AtomicI64::new(0));
  let registry = engine
    .register_activity(registry(&engine, &runs, 0), Refund)
    .unwrap();
  let i = id("both-1");
  db.run(engine.start::<ChargeAndRefund>(i.clone(), Order { id: 3 }));
  assert_eq!(db.run(engine.resume(&i)), Some(RunOutcome::Waiting));
  assert_eq!(db.drain(&registry, "w").succeeded, 1);
  assert_eq!(db.run(engine.resume(&i)), Some(RunOutcome::Waiting));
  // The activity queue's worker does not serve `payments`.
  assert_eq!(db.drain(&registry, "w").runs(), 0);
  let mut jobs = db.jobs();
  jobs.sort_by_key(|j| j.id);
  let rows: Vec<(&str, &str, Option<&str>)> = jobs
    .iter()
    .map(|j| {
      (
        j.name.as_str(),
        j.queue.as_str(),
        j.unique.as_ref().map(|u| u.key.as_str()),
      )
    })
    .collect();
  assert_eq!(
    rows,
    [
      ("charge", ACTIVITY_QUEUE, Some("both-1#1")),
      ("refund", "payments", Some("both-1#2"))
    ]
  );
  assert_eq!(
    jobs[1].payload,
    serde_json::json!({ "instance": "both-1", "seq": 2, "input": { "id": 3 } })
  );
  let config = WorkerConfig {
    id: WorkerId::new("payments"),
    clock: db.clock.clone(),
    ..WorkerConfig::default()
  };
  let env = hlist![JobQueueService::new(db.queue())];
  let payments = Worker::new(registry.clone())
    .queue("payments", 1)
    .with_config(config);
  assert_eq!(
    db.run(payments.run_until_idle().provide_all(env)).succeeded,
    1
  );
  assert_eq!(db.run(engine.resume(&i)), Some(RunOutcome::Completed));
  assert_eq!(
    db.run(engine.result::<ChargeAndRefund>(&i)),
    Some(Receipt {
      order: 3,
      charged: -42
    })
  );
}

#[test]
fn engines_and_workers_running_forever_complete_activities() {
  let test = "engines_and_workers_running_forever_complete_activities";
  let Some(db) = Db::new(test) else {
    return;
  };
  let runs = Arc::new(AtomicI64::new(0));
  let engines: Vec<WorkflowEngine> = ["a", "b"]
    .into_iter()
    .map(|name| db.engine(name).register(Checkout).unwrap())
    .collect();
  let registry = registry(&engines[0], &runs, 1);
  let ids: Vec<WorkflowId> = (0..6).map(|n| id(&format!("forever-{n}"))).collect();
  for (n, i) in ids.iter().enumerate() {
    db.run(engines[0].start::<Checkout>(i.clone(), Order { id: n as i64 }));
  }
  let mut fibers: Vec<_> = engines.iter().map(|e| db.rt.fork(e.run())).collect();
  let workers: Vec<_> = ["w1", "w2"]
    .into_iter()
    .map(|name| {
      let config = WorkerConfig {
        id: WorkerId::new(name),
        poll_interval: Duration::from_millis(20),
        clock: db.clock.clone(),
        ..WorkerConfig::default()
      };
      let env = hlist![JobQueueService::new(db.queue())];
      let worker = Worker::new(registry.clone())
        .queue(ACTIVITY_QUEUE, 2)
        .with_config(config);
      db.rt.fork(worker.run().provide_all(env))
    })
    .collect();
  let deadline = Instant::now() + Duration::from_secs(60);
  let done = loop {
    let finished = ids
      .iter()
      .filter(|i| db.run(engines[0].status(i)) == WorkflowStatus::Completed)
      .count();
    if finished == ids.len() || Instant::now() > deadline {
      break finished;
    }
    std::thread::sleep(Duration::from_millis(20));
  };
  for fiber in fibers.drain(..) {
    fiber.interrupt_now();
    let _ = db.rt.block_on(fiber.join_future());
  }
  for worker in workers {
    worker.interrupt_now();
    let _ = db.rt.block_on(worker.join_future());
  }
  assert_eq!(done, ids.len(), "every instance completed within 60 s");
  for (n, i) in ids.iter().enumerate() {
    let receipt = db.run(engines[1].result::<Checkout>(i)).unwrap();
    assert_eq!(receipt.order, n as i64);
    assert_eq!(db.run(db.store().history(i)).len(), 2);
  }
  assert!(db.jobs().iter().all(|j| j.state == JobState::Succeeded));
}

#[test]
fn activity_without_job_queue_keeps_the_instance() {
  let test = "activity_without_job_queue_keeps_the_instance";
  let Some(db) = Db::new(test) else {
    return;
  };
  let engine = WorkflowEngine::new(db.store())
    .with_clock(db.clock.clone())
    .register(Checkout)
    .unwrap();
  let i = id("no-queue");
  db.run(engine.start::<Checkout>(i.clone(), Order { id: 1 }));
  match db.rt.block_on(db.rt.run(engine.resume(&i))) {
    Exit::Failure(id_effect::Cause::Fail(e)) => assert_eq!(e, WorkflowError::NoJobQueue),
    other => panic!("expected NoJobQueue, got {other:?}"),
  }
  let record = db.run(engine.instance(&i)).unwrap();
  assert_eq!(record.status, WorkflowStatus::Running);
  assert!(record.lease_owner.is_none(), "the lease was given back");
}
