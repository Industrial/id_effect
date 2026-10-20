//! The engine on PostgreSQL with several writers: two engines never advance one instance at
//! once, a stale writer's append is rejected by the fencing token, and a crashed engine's
//! instance resumes from its history on another engine (with a new connection pool, as after a
//! process restart).
//!
//! The tests need a database at `DATABASE_URL`. When CI provides a Postgres service it also sets
//! `ID_EFFECT_REQUIRE_PG=1`, which turns a missing or unreachable database into a test
//! **failure**; without it they print an explicit `SKIP` line to stderr and return. Each test runs
//! in a schema of its own (`search_path`), so parallel test processes never share rows.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use id_effect::{Effect, Exit, Schema, UtcDateTime, WallClock, sleep, succeed, suspend, sync};
use id_effect_sql::SqlClient;
use id_effect_sql::pg::{PgPoolConfig, PgSqlClient, pg_pool_from_config};
use id_effect_tokio::TokioRuntime;
use id_effect_workflow::{
  EngineConfig, EngineId, EventKind, Fenced, Finish, HistoryEvent, NewInstance, PgWorkflowStore,
  RunOutcome, Workflow, WorkflowContext, WorkflowEngine, WorkflowError, WorkflowId, WorkflowStatus,
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
  /// `DATABASE_URL` scoped to the test's schema.
  scoped: String,
  client: PgSqlClient,
}

impl Db {
  /// A client on the fresh, migrated schema `wf_<test>`.
  fn new(test: &str) -> Option<Self> {
    let url = match std::env::var("DATABASE_URL") {
      Ok(url) => url,
      Err(_) => return unavailable(test, "DATABASE_URL is not set"),
    };
    let rt = TokioRuntime::new_multi_thread().expect("tokio runtime");
    let schema = format!("wf_{test}");
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
    let scoped = format!("{url}{sep}options=-c%20search_path%3Dwf_{test}");
    let client = Self::pool(&rt, &scoped);
    let db = Self { rt, scoped, client };
    let applied = db.run(
      migrator()
        .run_on(Arc::new(db.client.clone()))
        .map_err(|e| e.to_string()),
    );
    assert_eq!(applied, [1, 2]);
    Some(db)
  }

  fn pool(rt: &TokioRuntime, scoped: &str) -> PgSqlClient {
    let pool = rt
      .block_on(pg_pool_from_config(
        PgPoolConfig::from_url(scoped.to_owned()).with_max_size(16),
      ))
      .expect("pool on the test schema");
    PgSqlClient::new(pool)
  }

  /// A store on the shared pool.
  fn store(&self) -> PgWorkflowStore {
    PgWorkflowStore::new(self.client.clone())
  }

  /// A store on a pool of its own (another process).
  fn fresh_store(&self) -> PgWorkflowStore {
    PgWorkflowStore::new(Self::pool(&self.rt, &self.scoped))
  }

  fn run<A: Send + 'static, E: std::fmt::Debug + Send + 'static>(&self, eff: Effect<A, E>) -> A {
    match self.rt.block_on(self.rt.run(eff)) {
      Exit::Success(a) => a,
      Exit::Failure(c) => panic!("failed: {c:?}"),
    }
  }

  /// The typed failure of `eff`.
  fn fails<A: std::fmt::Debug + Send + 'static>(
    &self,
    eff: Effect<A, WorkflowError>,
  ) -> WorkflowError {
    match self.rt.block_on(self.rt.run(eff)) {
      Exit::Failure(id_effect::Cause::Fail(e)) => e,
      other => panic!("expected a typed failure, got {other:?}"),
    }
  }

  /// Polls `done` every 20 ms for at most 60 s.
  fn wait_until(&self, mut done: impl FnMut(&Db) -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
      if done(self) {
        return true;
      }
      std::thread::sleep(Duration::from_millis(20));
    }
    false
  }

  fn clock(&self) -> WallClock {
    WallClock::anchored(self.rt.now(), UtcDateTime::unsafe_make(1_700_000_000_000))
  }
}

fn id(s: &str) -> WorkflowId {
  WorkflowId::new(s).unwrap()
}

fn engine(
  store: PgWorkflowStore,
  name: &str,
  lease: Duration,
  clock: &WallClock,
) -> WorkflowEngine {
  WorkflowEngine::new(store)
    .with_config(EngineConfig {
      id: EngineId::new(name),
      lease,
      poll_interval: Duration::from_millis(20),
    })
    .with_clock(clock.clone())
}

#[derive(Clone, Debug, PartialEq, Schema)]
struct Count {
  n: i64,
}

/// Succeeds once `flag` is set (polling every 10 ms).
fn wait_for(flag: Arc<AtomicBool>) -> Effect<(), String> {
  suspend(move || {
    if flag.load(Ordering::SeqCst) {
      succeed(())
    } else {
      sleep(Duration::from_millis(10)).then(wait_for(flag))
    }
  })
}

/// Who runs which step of which instance right now, and what was ever seen.
#[derive(Default)]
struct Tracker {
  active: Mutex<BTreeMap<String, usize>>,
  overlaps: AtomicI64,
  runs: Mutex<BTreeMap<(String, i64), usize>>,
  engines: Mutex<BTreeSet<String>>,
}

/// Three steps of 20 ms each, every one recorded in the tracker.
struct Tracked {
  engine: &'static str,
  tracker: Arc<Tracker>,
}

impl Tracked {
  fn work(&self, instance: String, step: i64) -> Effect<Count, String> {
    let (tracker, engine) = (Arc::clone(&self.tracker), self.engine);
    let enter = {
      let (tracker, instance) = (Arc::clone(&tracker), instance.clone());
      sync(move || {
        let mut active = tracker.active.lock().unwrap();
        let n = active.entry(instance.clone()).or_default();
        if *n > 0 {
          tracker.overlaps.fetch_add(1, Ordering::SeqCst);
        }
        *n += 1;
        *tracker
          .runs
          .lock()
          .unwrap()
          .entry((instance, step))
          .or_default() += 1;
        tracker.engines.lock().unwrap().insert(engine.to_owned());
      })
    };
    enter
      .then(sleep(Duration::from_millis(20)))
      .then(sync(move || {
        *tracker.active.lock().unwrap().get_mut(&instance).unwrap() -= 1;
        Count { n: step }
      }))
  }
}

impl Workflow for Tracked {
  type Input = Count;
  type Output = Count;
  const NAME: &'static str = "tracked";

  fn run(&self, ctx: WorkflowContext, _: Count) -> Effect<Count, WorkflowError> {
    let instance = ctx.instance().to_string();
    let (c2, c3) = (ctx.clone(), ctx.clone());
    let (w2, w3) = (
      self.work(instance.clone(), 2),
      self.work(instance.clone(), 3),
    );
    ctx
      .step("one", self.work(instance, 1))
      .flat_map(move |_| c2.step("two", w2))
      .flat_map(move |_| c3.step("three", w3))
  }
}

#[test]
fn two_engines_never_advance_one_instance_concurrently() {
  let test = "two_engines_never_advance_one_instance_concurrently";
  let Some(db) = Db::new(test) else {
    return;
  };
  let clock = WallClock::system();
  let tracker = Arc::new(Tracker::default());
  let make = |name: &'static str| {
    engine(db.store(), name, Duration::from_secs(3), &clock)
      .register(Tracked {
        engine: name,
        tracker: Arc::clone(&tracker),
      })
      .unwrap()
  };
  let (a, b) = (make("a"), make("b"));

  // Eight engines race to acquire one fresh instance: exactly one wins.
  let race = id("race");
  db.run(a.start::<Tracked>(race.clone(), Count { n: 0 }));
  let store = db.store();
  let attempts: Vec<_> = (0..8)
    .map(|n| {
      let (store, race) = (store.clone(), race.clone());
      let now = UtcDateTime::unsafe_make(1_700_000_000_000);
      let until = UtcDateTime::unsafe_make(1_700_000_030_000);
      db.rt.fork(suspend(move || {
        store.acquire(&race, &EngineId::new(format!("racer-{n}")), now, until)
      }))
    })
    .collect();
  let winners = attempts
    .into_iter()
    .map(|h| db.rt.block_on(h.join_future()))
    .filter(|exit| matches!(exit, Exit::Success(Some(_))))
    .count();
  assert_eq!(winners, 1, "one lease per instance");
  assert_eq!(db.run(store.get(&race)).unwrap().fencing_token, 1);

  let mut ids: Vec<WorkflowId> = (0..8).map(|n| id(&format!("tracked-{n}"))).collect();
  for i in &ids {
    db.run(a.start::<Tracked>(i.clone(), Count { n: 0 }));
  }
  // The racer's lease (at a 2023 instant) has expired on the system clock: it is taken over.
  ids.push(race);
  // Two drivers per engine, all four competing for the same nine instances.
  let fibers: Vec<_> = [&a, &a, &b, &b]
    .into_iter()
    .map(|e| db.rt.fork(e.run()))
    .collect();
  let done = db.wait_until(|db| {
    ids
      .iter()
      .all(|i| db.run(store.get(i)).unwrap().status == WorkflowStatus::Completed)
  });
  for fiber in &fibers {
    fiber.interrupt_now();
  }
  for fiber in fibers {
    assert!(matches!(
      db.rt.block_on(fiber.join_future()),
      Exit::Failure(_)
    ));
  }
  assert!(done, "every instance completed within 60 s");

  assert_eq!(
    tracker.overlaps.load(Ordering::SeqCst),
    0,
    "never two runs of one instance at once"
  );
  let runs = tracker.runs.lock().unwrap().clone();
  assert_eq!(runs.len(), 9 * 3);
  assert!(
    runs.values().all(|&n| n == 1),
    "every step ran once: {runs:?}"
  );
  for i in &ids {
    let history = db.run(store.history(i));
    let seqs: Vec<i64> = history.iter().map(|e| e.seq).collect();
    assert_eq!(seqs, [1, 2, 3]);
    assert_eq!(db.run(a.result::<Tracked>(i)), Some(Count { n: 3 }));
  }
  eprintln!(
    "engines that ran steps: {:?}",
    tracker.engines.lock().unwrap()
  );
}

/// One step that returns the engine's number; engine 1's step waits for `gate`.
struct Gated {
  number: i64,
  gate: Arc<AtomicBool>,
  entered: Arc<AtomicBool>,
}

impl Workflow for Gated {
  type Input = Count;
  type Output = Count;
  const NAME: &'static str = "gated";

  fn run(&self, ctx: WorkflowContext, _: Count) -> Effect<Count, WorkflowError> {
    let (gate, entered, number) = (
      Arc::clone(&self.gate),
      Arc::clone(&self.entered),
      self.number,
    );
    let work = sync(move || entered.store(true, Ordering::SeqCst))
      .then(wait_for(gate))
      .map(move |()| Count { n: number });
    ctx.step("work", work)
  }
}

fn event(seq: i64, at: UtcDateTime) -> HistoryEvent {
  HistoryEvent {
    seq,
    kind: EventKind::Step,
    name: format!("s{seq}"),
    payload: serde_json::json!({ "ok": { "n": seq } }),
    recorded_at: at,
  }
}

#[test]
fn stale_writer_append_is_rejected_by_fencing() {
  let test = "stale_writer_append_is_rejected_by_fencing";
  let Some(db) = Db::new(test) else {
    return;
  };
  let store = db.store();
  let t = |secs: i64| UtcDateTime::unsafe_make(1_700_000_000_000 + secs * 1000);

  // Store level: a's lease runs out, b takes over, every write of a is refused from then on.
  let i = id("store-level");
  let new = NewInstance {
    id: i.clone(),
    name: "w".into(),
    version: "1".into(),
    input: serde_json::json!({}),
  };
  assert!(db.run(store.create(new.clone(), t(0))));
  assert!(!db.run(store.create(new, t(0))));
  let (ea, eb) = (EngineId::new("a"), EngineId::new("b"));
  let la = db.run(store.acquire(&i, &ea, t(0), t(10))).unwrap().lease;
  assert_eq!(la.token, 1);
  assert!(
    db.run(store.acquire(&i, &eb, t(5), t(15))).is_none(),
    "held by a"
  );
  assert_eq!(
    db.run(store.append(&la, event(1, t(1)), t(11))),
    Fenced::Applied
  );
  assert_eq!(
    db.run(store.append(&la, event(1, t(2)), t(12))),
    Fenced::Lost,
    "seq 1 is taken"
  );
  assert_eq!(
    db.run(store.append(&la, event(2, t(13)), t(23))),
    Fenced::Lost,
    "a's lease ran out"
  );
  let lb = db
    .run(store.claim("w", &eb, 5, t(13), t(23)))
    .remove(0)
    .lease;
  assert_eq!(lb.token, 2);
  // a's clock may lag: even "before its expiry" its token is stale.
  assert_eq!(
    db.run(store.append(&la, event(2, t(5)), t(15))),
    Fenced::Lost
  );
  assert_eq!(db.run(store.renew(&la, t(5), t(15))), Fenced::Lost);
  assert_eq!(
    db.run(store.finish(&la, Finish::Failed("stale".into()), t(5))),
    Fenced::Lost
  );
  assert_eq!(db.run(store.release(&la, t(5))), Fenced::Lost);
  assert_eq!(
    db.run(store.append(&lb, event(2, t(14)), t(24))),
    Fenced::Applied
  );
  let seqs: Vec<i64> = db.run(store.history(&i)).iter().map(|e| e.seq).collect();
  assert_eq!(seqs, [1, 2]);
  let record = db.run(store.get(&i)).unwrap();
  assert_eq!((record.lease_owner, record.fencing_token), (Some(eb), 2));
  assert_eq!(
    record.lease_expires_at,
    Some(t(24)),
    "b's append extended its lease"
  );

  // Engine level: a's step is in flight when b takes the instance over; a's result is never
  // journaled and a's run ends with `LeaseLost`.
  let clock = db.clock();
  let gate = Arc::new(AtomicBool::new(false));
  let entered = Arc::new(AtomicBool::new(false));
  let a = engine(db.store(), "a", Duration::from_secs(10), &clock)
    .register(Gated {
      number: 1,
      gate: Arc::clone(&gate),
      entered: Arc::clone(&entered),
    })
    .unwrap();
  let b = engine(db.store(), "b", Duration::from_secs(10), &clock)
    .register(Gated {
      number: 2,
      gate: Arc::new(AtomicBool::new(true)),
      entered: Arc::new(AtomicBool::new(false)),
    })
    .unwrap();
  let g = id("engine-level");
  db.run(a.start::<Gated>(g.clone(), Count { n: 0 }));
  let run_a = db.rt.fork(a.resume(&g));
  assert!(
    db.wait_until(|_| entered.load(Ordering::SeqCst)),
    "a is in its step"
  );
  assert_eq!(db.run(b.resume(&g)), None, "a's lease is live");
  clock.jump(Duration::from_secs(11));
  assert_eq!(db.run(b.resume(&g)), Some(RunOutcome::Completed));
  gate.store(true, Ordering::SeqCst);
  assert!(matches!(
    db.rt.block_on(run_a.join_future()),
    Exit::Success(Some(RunOutcome::LeaseLost))
  ));
  let history = db.run(store.history(&g));
  assert_eq!(history.len(), 1);
  assert_eq!(
    history[0].payload,
    serde_json::json!({ "ok": { "n": 2 } }),
    "b's result"
  );
  assert_eq!(db.run(a.result::<Gated>(&g)), Some(Count { n: 2 }));
}

/// Step one counts, step two waits for `gate`.
struct TwoSteps {
  ones: Arc<AtomicI64>,
  gate: Arc<AtomicBool>,
}

impl Workflow for TwoSteps {
  type Input = Count;
  type Output = Count;
  const NAME: &'static str = "two-steps";

  fn run(&self, ctx: WorkflowContext, input: Count) -> Effect<Count, WorkflowError> {
    let ones = Arc::clone(&self.ones);
    let one: Effect<Count, String> = sync(move || Count {
      n: input.n + ones.fetch_add(1, Ordering::SeqCst) + 1,
    });
    let two = wait_for(Arc::clone(&self.gate)).map(|()| Count { n: 10 });
    let c2 = ctx.clone();
    ctx.step("one", one).flat_map(move |first| {
      c2.step("two", two).map(move |second| Count {
        n: first.n + second.n,
      })
    })
  }
}

#[test]
fn crashed_engine_instance_resumes_elsewhere() {
  let test = "crashed_engine_instance_resumes_elsewhere";
  let Some(db) = Db::new(test) else {
    return;
  };
  let clock = db.clock();
  let ones = Arc::new(AtomicI64::new(0));
  let a = engine(db.store(), "a", Duration::from_secs(10), &clock)
    .register(TwoSteps {
      ones: Arc::clone(&ones),
      gate: Arc::new(AtomicBool::new(false)),
    })
    .unwrap();
  let i = id("crash-1");
  db.run(a.start::<TwoSteps>(i.clone(), Count { n: 5 }));
  let store = db.store();
  let fiber = db.rt.fork(a.run());
  let one_journaled = db.wait_until(|db| db.run(store.history(&i)).len() == 1);
  // The crash: a stops mid-step two, without releasing its lease.
  fiber.interrupt_now();
  assert!(matches!(
    db.rt.block_on(fiber.join_future()),
    Exit::Failure(_)
  ));
  assert!(one_journaled, "step one journaled within 60 s");
  let held = db.run(store.get(&i)).unwrap();
  assert_eq!(
    (held.lease_owner, held.fencing_token),
    (Some(EngineId::new("a")), 1)
  );

  // Another engine in another "process" (its own pool), same database.
  let b = engine(db.fresh_store(), "b", Duration::from_secs(10), &clock)
    .register(TwoSteps {
      ones: Arc::clone(&ones),
      gate: Arc::new(AtomicBool::new(true)),
    })
    .unwrap();
  assert_eq!(db.run(b.run_once()), 0, "a's lease has not run out");
  clock.jump(Duration::from_secs(11));
  assert_eq!(db.run(b.run_once()), 1);

  assert_eq!(
    ones.load(Ordering::SeqCst),
    1,
    "step one was replayed, not re-run"
  );
  assert_eq!(db.run(b.status(&i)), WorkflowStatus::Completed);
  assert_eq!(db.run(b.result::<TwoSteps>(&i)), Some(Count { n: 16 }));
  let record = db.run(store.get(&i)).unwrap();
  assert_eq!((record.fencing_token, record.lease_owner), (2, None));
  assert_eq!(record.output, Some(serde_json::json!({ "n": 16 })));
  let kinds: Vec<String> = db
    .run(store.history(&i))
    .into_iter()
    .map(|e| e.name)
    .collect();
  assert_eq!(kinds, ["one", "two"]);
}

#[test]
fn engine_failures_are_typed_on_pg() {
  let test = "engine_failures_are_typed_on_pg";
  let Some(db) = Db::new(test) else {
    return;
  };
  let clock = db.clock();
  let e = engine(db.store(), "e", Duration::from_secs(10), &clock);
  assert_eq!(
    db.fails(e.status(&id("missing"))),
    WorkflowError::NotFound("missing".into())
  );
  db.run(e.start::<TwoSteps>(id("x"), Count { n: 1 }));
  assert_eq!(
    db.fails(e.start::<Gated>(id("x"), Count { n: 1 })),
    WorkflowError::IdInUse {
      id: "x".into(),
      workflow: "two-steps".into()
    }
  );
  assert_eq!(db.run(e.result::<TwoSteps>(&id("x"))), None);
}
