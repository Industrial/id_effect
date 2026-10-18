//! Signals on PostgreSQL (fault.workflows#5): `WorkflowClient::signal` stores a signal and wakes
//! the instance, `ctx.wait_signal` receives the signals of one name in arrival order, and a
//! signal sent before the wait is buffered.
//!
//! The tests need a database at `DATABASE_URL`. When CI provides a Postgres service it also sets
//! `ID_EFFECT_REQUIRE_PG=1`, which turns a missing or unreachable database into a test
//! **failure**; without it they print an explicit `SKIP` line to stderr and return. Each test runs
//! in a schema of its own (`search_path`), so parallel test processes never share rows.

use std::sync::Arc;
use std::time::Duration;

use id_effect::{Effect, Exit, Schema, UtcDateTime, WallClock, succeed};
use id_effect_sql::SqlClient;
use id_effect_sql::pg::{PgPoolConfig, PgSqlClient, pg_pool_from_config};
use id_effect_tokio::TokioRuntime;
use id_effect_workflow::{
  EngineConfig, EngineId, EventKind, PgWorkflowStore, RunOutcome, Workflow, WorkflowClient,
  WorkflowContext, WorkflowEngine, WorkflowError, WorkflowId, WorkflowNameError, WorkflowStatus,
  WorkflowStore, migrator,
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
  clock: WallClock,
}

impl Db {
  /// A client on the fresh, migrated schema `sig_<test>`.
  fn new(test: &str) -> Option<Self> {
    let url = match std::env::var("DATABASE_URL") {
      Ok(url) => url,
      Err(_) => return unavailable(test, "DATABASE_URL is not set"),
    };
    let rt = TokioRuntime::new_multi_thread().expect("tokio runtime");
    let schema = format!("sig_{test}");
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
    let applied = db.run(
      migrator()
        .run_on(Arc::new(db.client.clone()))
        .map_err(|e| e.to_string()),
    );
    assert_eq!(applied, [1, 2]);
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

  fn engine(&self) -> WorkflowEngine {
    WorkflowEngine::new(self.store())
      .with_config(EngineConfig {
        id: EngineId::new("e"),
        lease: Duration::from_secs(30),
        poll_interval: Duration::from_millis(20),
      })
      .with_clock(self.clock.clone())
      .register(Approval)
      .unwrap()
      .register(Collect)
      .unwrap()
  }
}

fn id(s: &str) -> WorkflowId {
  WorkflowId::new(s).unwrap()
}

#[derive(Clone, Debug, PartialEq, Schema)]
struct Count {
  n: i64,
}

#[derive(Clone, Debug, PartialEq, Schema)]
struct Items {
  values: Vec<i64>,
}

/// Waits for one `approve` signal and returns it.
struct Approval;

impl Workflow for Approval {
  type Input = Count;
  type Output = Count;
  const NAME: &'static str = "approval";

  fn run(&self, ctx: WorkflowContext, _: Count) -> Effect<Count, WorkflowError> {
    ctx.wait_signal::<Count>("approve")
  }
}

/// Receives `n` `item` signals, in order.
struct Collect;

fn collect(ctx: WorkflowContext, left: i64, mut got: Vec<i64>) -> Effect<Items, WorkflowError> {
  if left == 0 {
    return succeed(Items { values: got });
  }
  let next = ctx.clone();
  ctx.wait_signal::<Count>("item").flat_map(move |item| {
    got.push(item.n);
    collect(next, left - 1, got)
  })
}

impl Workflow for Collect {
  type Input = Count;
  type Output = Items;
  const NAME: &'static str = "collect";

  fn run(&self, ctx: WorkflowContext, n: Count) -> Effect<Items, WorkflowError> {
    collect(ctx, n.n, Vec::new())
  }
}

#[test]
fn signal_before_wait_is_buffered() {
  let test = "signal_before_wait_is_buffered";
  let Some(db) = Db::new(test) else {
    return;
  };
  let engine = db.engine();
  let client = engine.client();

  // Sent before the instance ever ran: buffered, received at once.
  let early = id("approval-early");
  db.run(engine.start::<Approval>(early.clone(), Count { n: 0 }));
  assert!(db.run(client.signal(&early, "approve", Count { n: 5 })));
  assert_eq!(db.run(engine.resume(&early)), Some(RunOutcome::Completed));
  assert_eq!(
    db.run(engine.result::<Approval>(&early)),
    Some(Count { n: 5 })
  );
  let history = db.run(db.store().history(&early));
  assert_eq!((history.len(), history[0].kind), (1, EventKind::Signal));
  assert_eq!(history[0].name, "approve");
  assert_eq!(
    history[0].payload,
    serde_json::json!({ "number": 1, "payload": { "n": 5 } })
  );
  // A finished or absent instance takes no signal.
  assert!(!db.run(client.signal(&early, "approve", Count { n: 6 })));
  assert!(!db.run(client.signal(&id("absent"), "approve", Count { n: 6 })));

  // Sent while the instance waits: stored, and it wakes the parked instance.
  let late = id("approval-late");
  db.run(engine.start::<Approval>(late.clone(), Count { n: 0 }));
  assert_eq!(db.run(engine.resume(&late)), Some(RunOutcome::Waiting));
  assert!(db.run(engine.instance(&late)).unwrap().waiting);
  assert_eq!(db.run(engine.resume(&late)), None, "parked");
  // A client of its own (another process) sends it.
  let other =
    WorkflowClient::new(PgWorkflowStore::new(db.client.clone())).with_clock(db.clock.clone());
  assert!(db.run(other.signal(&late, "approve", Count { n: 7 })));
  let woken = db.run(engine.instance(&late)).unwrap();
  assert!(!woken.waiting && woken.wakeups == 1);
  assert_eq!(db.run(engine.resume(&late)), Some(RunOutcome::Completed));
  assert_eq!(
    db.run(engine.result::<Approval>(&late)),
    Some(Count { n: 7 })
  );

  // A signal name must be a valid name.
  let invalid = db
    .rt
    .block_on(db.rt.run(client.signal(&late, "no spaces", Count { n: 1 })));
  assert!(matches!(
    invalid,
    Exit::Failure(id_effect::Cause::Fail(WorkflowError::InvalidName(
      WorkflowNameError::InvalidChar { .. }
    )))
  ));
}

/// Waits for `b`, then for `a`.
struct TwoNames;

impl Workflow for TwoNames {
  type Input = Count;
  type Output = Items;
  const NAME: &'static str = "two-names";

  fn run(&self, ctx: WorkflowContext, _: Count) -> Effect<Items, WorkflowError> {
    let next = ctx.clone();
    ctx.wait_signal::<Count>("b").flat_map(move |b| {
      next.wait_signal::<Count>("a").map(move |a| Items {
        values: vec![b.n, a.n],
      })
    })
  }
}

#[test]
fn signal_names_are_separate_buffers() {
  let test = "signal_names_are_separate_buffers";
  let Some(db) = Db::new(test) else {
    return;
  };
  let engine = db.engine().register(TwoNames).unwrap();
  let client = engine.client();
  let i = id("two-names");
  db.run(engine.start::<TwoNames>(i.clone(), Count { n: 0 }));
  // `a` arrives first but is only taken by the second wait.
  assert!(db.run(client.signal(&i, "a", Count { n: 1 })));
  assert!(db.run(client.signal(&i, "a", Count { n: 2 })));
  assert_eq!(db.run(engine.resume(&i)), Some(RunOutcome::Waiting));
  assert!(db.run(client.signal(&i, "b", Count { n: 3 })));
  assert_eq!(db.run(engine.resume(&i)), Some(RunOutcome::Completed));
  assert_eq!(
    db.run(engine.result::<TwoNames>(&i)),
    Some(Items { values: vec![3, 1] })
  );
  let numbers: Vec<i64> = db
    .run(db.store().history(&i))
    .iter()
    .map(|e| e.payload["number"].as_i64().unwrap())
    .collect();
  assert_eq!(numbers, [3, 1], "b's number, then a's first");
  // The second `a` stays stored, unconsumed.
  let left = db.run(db.store().next_signal(&i, "a", 1)).unwrap();
  assert_eq!(
    (left.number, left.payload.clone()),
    (2, serde_json::json!({ "n": 2 }))
  );
}

#[test]
fn a_signal_wakes_the_running_engines() {
  let test = "a_signal_wakes_the_running_engines";
  let Some(db) = Db::new(test) else {
    return;
  };
  let engine = db.engine();
  let i = id("approval-run");
  db.run(engine.start::<Approval>(i.clone(), Count { n: 0 }));
  let fiber = db.rt.fork(engine.run());
  let deadline = std::time::Instant::now() + Duration::from_secs(60);
  while !db.run(engine.instance(&i)).unwrap().waiting && std::time::Instant::now() < deadline {
    std::thread::sleep(Duration::from_millis(10));
  }
  assert!(db.run(engine.client().signal(&i, "approve", Count { n: 11 })));
  while db.run(engine.status(&i)) != WorkflowStatus::Completed
    && std::time::Instant::now() < deadline
  {
    std::thread::sleep(Duration::from_millis(10));
  }
  fiber.interrupt_now();
  let _ = db.rt.block_on(fiber.join_future());
  assert_eq!(db.run(engine.result::<Approval>(&i)), Some(Count { n: 11 }));
}

#[test]
fn a_signal_that_does_not_decode_fails_the_instance() {
  let test = "a_signal_that_does_not_decode_fails_the_instance";
  let Some(db) = Db::new(test) else {
    return;
  };
  let engine = db.engine();
  let i = id("approval-bad");
  db.run(engine.start::<Approval>(i.clone(), Count { n: 0 }));
  assert!(
    db.run(
      engine
        .client()
        .signal(&i, "approve", Items { values: vec![] })
    )
  );
  match db.run(engine.resume(&i)) {
    Some(RunOutcome::Failed(WorkflowError::Decode { what, .. })) => {
      assert_eq!(what, "signal approve");
    }
    other => panic!("expected a decode failure, got {other:?}"),
  }
  assert_eq!(db.run(engine.status(&i)), WorkflowStatus::Failed);
}

#[test]
fn signals_delivered_in_order() {
  let test = "signals_delivered_in_order";
  let Some(db) = Db::new(test) else {
    return;
  };
  let engine = db.engine();
  let client = engine.client();

  // One before the first run, then two more (with a signal of another name in between) while
  // the instance waits.
  let i = id("collect-3");
  db.run(engine.start::<Collect>(i.clone(), Count { n: 3 }));
  assert!(db.run(client.signal(&i, "item", Count { n: 1 })));
  assert_eq!(db.run(engine.resume(&i)), Some(RunOutcome::Waiting));
  assert!(db.run(client.signal(&i, "other", Count { n: 100 })));
  assert!(db.run(client.signal(&i, "item", Count { n: 2 })));
  assert!(db.run(client.signal(&i, "item", Count { n: 3 })));
  assert_eq!(db.run(engine.resume(&i)), Some(RunOutcome::Completed));
  assert_eq!(
    db.run(engine.result::<Collect>(&i)),
    Some(Items {
      values: vec![1, 2, 3]
    })
  );
  let numbers: Vec<i64> = db
    .run(db.store().history(&i))
    .iter()
    .map(|e| e.payload["number"].as_i64().unwrap())
    .collect();
  assert_eq!(numbers, [1, 3, 4], "the `other` signal took number 2");

  // Twenty concurrent senders: every signal is received once, in the order the numbers were
  // assigned (commit order).
  let j = id("collect-20");
  db.run(engine.start::<Collect>(j.clone(), Count { n: 20 }));
  let sends: Vec<_> = (1..=20)
    .map(|n| {
      let (client, j) = (client.clone(), j.clone());
      db.rt.fork(id_effect::suspend(move || {
        client.signal(&j, "item", Count { n })
      }))
    })
    .collect();
  for send in sends {
    assert!(matches!(
      db.rt.block_on(send.join_future()),
      Exit::Success(true)
    ));
  }
  assert_eq!(db.run(engine.resume(&j)), Some(RunOutcome::Completed));
  let got = db.run(engine.result::<Collect>(&j)).unwrap().values;
  let mut sorted = got.clone();
  sorted.sort_unstable();
  assert_eq!(sorted, (1..=20).collect::<Vec<_>>(), "each signal once");
  let history = db.run(db.store().history(&j));
  let numbers: Vec<i64> = history
    .iter()
    .map(|e| e.payload["number"].as_i64().unwrap())
    .collect();
  assert!(
    numbers.windows(2).all(|w| w[0] < w[1]),
    "arrival order: {numbers:?}"
  );
  let payloads: Vec<i64> = history
    .iter()
    .map(|e| e.payload["payload"]["n"].as_i64().unwrap())
    .collect();
  assert_eq!(payloads, got);
  assert_eq!(db.run(engine.status(&j)), WorkflowStatus::Completed);
}
