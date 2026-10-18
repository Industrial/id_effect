//! Journal replay on the in-memory store: journaled steps, `now` and `random` come back on resume
//! without re-running, a crash re-runs only the step in flight, a diverging replay is a typed
//! `NonDeterministic` failure, and the start / status / result API. `engine_pg.rs` runs the
//! multi-writer cases on PostgreSQL.

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use id_effect::{
  Cause, Effect, Exit, Random, Schema, UtcDateTime, WallClock, die, fail, succeed, sync,
};
use id_effect_workflow::{
  EngineConfig, EngineId, EventKind, MemoryWorkflowStore, RunOutcome, Started, Workflow,
  WorkflowContext, WorkflowEngine, WorkflowError, WorkflowId, WorkflowStatus, WorkflowStore,
};

#[derive(Clone, Debug, PartialEq, Schema)]
struct Count {
  n: i64,
}

#[derive(Clone, Debug, PartialEq, Schema)]
struct Totals {
  first: i64,
  second: i64,
}

fn ok<A: Send + 'static>(eff: Effect<A, WorkflowError>) -> A {
  match eff.block_on() {
    Exit::Success(a) => a,
    Exit::Failure(c) => panic!("unexpected failure: {c:?}"),
  }
}

fn err<A: std::fmt::Debug + Send + 'static>(eff: Effect<A, WorkflowError>) -> WorkflowError {
  match eff.block_on() {
    Exit::Failure(Cause::Fail(e)) => e,
    other => panic!("expected a typed failure, got {other:?}"),
  }
}

fn id(s: &str) -> WorkflowId {
  WorkflowId::new(s).unwrap()
}

fn clock() -> WallClock {
  WallClock::anchored(Instant::now(), UtcDateTime::unsafe_make(1_700_000_000_000))
}

fn engine(store: &MemoryWorkflowStore, name: &str, clock: &WallClock, seed: u64) -> WorkflowEngine {
  WorkflowEngine::new(store.clone())
    .with_config(EngineConfig {
      id: EngineId::new(name),
      lease: Duration::from_secs(30),
      ..EngineConfig::default()
    })
    .with_clock(clock.clone())
    .with_random(Random::seeded(seed))
}

/// Counts how often each step's effect ran; step `two` dies once while `crash` is set.
#[derive(Clone, Default)]
struct Probe {
  ones: Arc<AtomicI64>,
  twos: Arc<AtomicI64>,
  crash: Arc<AtomicBool>,
  /// The `(now, random)` each run of the body saw.
  seen: Arc<Mutex<Vec<(UtcDateTime, u64)>>>,
}

impl Workflow for Probe {
  type Input = Count;
  type Output = Totals;
  const NAME: &'static str = "probe";

  fn run(&self, ctx: WorkflowContext, input: Count) -> Effect<Totals, WorkflowError> {
    let probe = self.clone();
    let (c1, c2) = (ctx.clone(), ctx.clone());
    ctx.now().flat_map(move |at| {
      c1.random().flat_map(move |r| {
        probe.seen.lock().unwrap().push((at, r));
        let ones = Arc::clone(&probe.ones);
        let one: Effect<Count, String> = sync(move || Count {
          n: input.n + 100 * (ones.fetch_add(1, Ordering::SeqCst) + 1),
        });
        c2.step("one", one).flat_map(move |first| {
          let two: Effect<Count, String> = if probe.crash.swap(false, Ordering::SeqCst) {
            die("the engine crashed in step two")
          } else {
            let twos = Arc::clone(&probe.twos);
            sync(move || Count {
              n: twos.fetch_add(1, Ordering::SeqCst) + 1,
            })
          };
          c2.step("two", two).map(move |second| Totals {
            first: first.n,
            second: second.n,
          })
        })
      })
    })
  }
}

#[test]
fn replay_returns_journaled_results_without_rerunning_steps() {
  let (store, clock) = (MemoryWorkflowStore::new(), clock());
  let probe = Probe::default();
  probe.crash.store(true, Ordering::SeqCst);
  let a = engine(&store, "a", &clock, 1)
    .register(probe.clone())
    .unwrap();
  let b = engine(&store, "b", &clock, 2)
    .register(probe.clone())
    .unwrap();
  let i = id("probe-1");
  assert_eq!(
    ok(a.start::<Probe>(i.clone(), Count { n: 7 })),
    Started::New(i.clone())
  );

  // Engine a dies inside step two: `now`, `random` and step one are journaled, two is not.
  assert!(matches!(
    a.resume(&i).block_on(),
    Exit::Failure(Cause::Die(_))
  ));
  let kinds: Vec<(EventKind, String)> = ok(store.history(&i))
    .into_iter()
    .map(|e| (e.kind, e.name))
    .collect();
  assert_eq!(
    kinds,
    [
      (EventKind::Now, "now".to_owned()),
      (EventKind::Random, "random".to_owned()),
      (EventKind::Step, "one".to_owned()),
    ]
  );
  assert_eq!(ok(b.status(&i)), WorkflowStatus::Running);
  assert_eq!(ok(b.result::<Probe>(&i)), None, "still running");

  // Its lease still runs: b cannot take the instance yet.
  assert_eq!(ok(b.resume(&i)), None);
  clock.jump(Duration::from_secs(31));
  assert_eq!(ok(b.resume(&i)), Some(RunOutcome::Completed));

  assert_eq!(probe.ones.load(Ordering::SeqCst), 1, "step one ran once");
  assert_eq!(
    probe.twos.load(Ordering::SeqCst),
    1,
    "step two ran after the crash only"
  );
  let seen = probe.seen.lock().unwrap().clone();
  assert_eq!(seen.len(), 2, "the body ran on a and again on b");
  assert_eq!(
    seen[0], seen[1],
    "now and random replayed, though b's clock and seed differ"
  );
  assert_eq!(
    ok(b.result::<Probe>(&i)),
    Some(Totals {
      first: 107,
      second: 1
    })
  );
  assert_eq!(ok(store.history(&i)).len(), 4);
  assert_eq!(ok(b.instance(&i)).unwrap().lease_owner, None);

  // Finished: nobody resumes it again.
  clock.jump(Duration::from_secs(60));
  assert_eq!(ok(a.resume(&i)), None);
  assert_eq!(probe.ones.load(Ordering::SeqCst), 1);
}

/// One workflow name, several bodies: the plan picks what the code asks for.
#[derive(Clone, Copy)]
enum Plan {
  /// Step `a`, then dies in step `b`.
  CrashInB,
  /// Step `x` first.
  RenamedFirstStep,
  /// `now` first.
  NowFirst,
  /// No steps at all.
  Nothing,
}

struct Orders(Plan);

impl Workflow for Orders {
  type Input = Count;
  type Output = Count;
  const NAME: &'static str = "orders";
  const VERSION: &'static str = "2";

  fn run(&self, ctx: WorkflowContext, input: Count) -> Effect<Count, WorkflowError> {
    let step = |name: &str, n: i64| ctx.step(name, succeed::<Count, String, _>(Count { n }));
    match self.0 {
      Plan::CrashInB => {
        let ctx2 = ctx.clone();
        step("a", input.n).flat_map(move |_| ctx2.step("b", die::<Count, String, _>("crash")))
      }
      Plan::RenamedFirstStep => step("x", input.n),
      Plan::NowFirst => ctx.now().map(move |_| input),
      Plan::Nothing => succeed(input),
    }
  }
}

#[test]
fn nondeterministic_replay_is_detected() {
  let (store, clock) = (MemoryWorkflowStore::new(), clock());
  let first = engine(&store, "first", &clock, 1)
    .register(Orders(Plan::CrashInB))
    .unwrap();
  for n in 1..=3 {
    let i = id(&format!("order-{n}"));
    ok(first.start::<Orders>(i.clone(), Count { n }));
    assert!(matches!(
      first.resume(&i).block_on(),
      Exit::Failure(Cause::Die(_))
    ));
    assert_eq!(ok(store.history(&i)).len(), 1, "step a journaled");
    assert_eq!(
      ok(store.get(&i)).unwrap().version,
      "2",
      "the version is recorded"
    );
  }
  clock.jump(Duration::from_secs(31));

  let cases = [
    (Plan::RenamedFirstStep, "step x"),
    (Plan::NowFirst, "now"),
    (Plan::Nothing, "the end of the workflow"),
  ];
  for (n, (plan, code)) in (1..).zip(cases) {
    let i = id(&format!("order-{n}"));
    let changed = engine(&store, &format!("changed-{n}"), &clock, 1)
      .register(Orders(plan))
      .unwrap();
    let expected = WorkflowError::NonDeterministic {
      seq: 1,
      history: "step a".into(),
      code: code.into(),
    };
    assert_eq!(
      ok(changed.resume(&i)),
      Some(RunOutcome::Failed(expected.clone()))
    );
    assert_eq!(ok(changed.status(&i)), WorkflowStatus::Failed);
    assert_eq!(
      err(changed.result::<Orders>(&i)),
      WorkflowError::Failed(expected.to_string())
    );
    assert_eq!(ok(store.history(&i)).len(), 1, "nothing appended");
  }
}

/// Charges once; the charge fails with a typed error.
struct Declined {
  attempts: Arc<AtomicI64>,
}

impl Workflow for Declined {
  type Input = Count;
  type Output = Count;
  const NAME: &'static str = "declined";

  fn run(&self, ctx: WorkflowContext, _: Count) -> Effect<Count, WorkflowError> {
    let attempts = Arc::clone(&self.attempts);
    let charge: Effect<Count, String> = sync(move || attempts.fetch_add(1, Ordering::SeqCst))
      .flat_map(|_| fail("card declined".to_owned()));
    ctx.step("charge", charge)
  }
}

#[test]
fn a_step_failure_is_journaled_and_fails_the_instance() {
  let (store, clock) = (MemoryWorkflowStore::new(), clock());
  let attempts = Arc::new(AtomicI64::new(0));
  let e = engine(&store, "e", &clock, 1)
    .register(Declined {
      attempts: Arc::clone(&attempts),
    })
    .unwrap();
  let i = id("declined-1");
  ok(e.start::<Declined>(i.clone(), Count { n: 1 }));
  let step = WorkflowError::Step {
    step: "charge".into(),
    message: "card declined".into(),
  };
  assert_eq!(ok(e.resume(&i)), Some(RunOutcome::Failed(step.clone())));
  let history = ok(store.history(&i));
  assert_eq!(
    history[0].payload,
    serde_json::json!({ "err": "card declined" })
  );
  assert_eq!(
    err(e.result::<Declined>(&i)),
    WorkflowError::Failed(step.to_string())
  );
  assert_eq!(attempts.load(Ordering::SeqCst), 1);
}

#[test]
fn start_is_idempotent_and_reads_report_missing_or_foreign_ids() {
  let (store, clock) = (MemoryWorkflowStore::new(), clock());
  let e = engine(&store, "e", &clock, 1)
    .register(Orders(Plan::Nothing))
    .unwrap();
  let i = id("same");
  assert_eq!(
    ok(e.start::<Orders>(i.clone(), Count { n: 1 })),
    Started::New(i.clone())
  );
  let again = ok(e.start::<Orders>(i.clone(), Count { n: 2 }));
  assert_eq!((again.id(), &again), (&i, &Started::Existing(i.clone())));
  assert_eq!(
    ok(store.get(&i)).unwrap().input,
    serde_json::json!({ "n": 1 })
  );
  assert!(matches!(
    err(e.start::<Probe>(i.clone(), Count { n: 1 })),
    WorkflowError::IdInUse { .. }
  ));
  assert!(matches!(
    err(e.result::<Probe>(&i)),
    WorkflowError::IdInUse { .. }
  ));
  assert_eq!(
    err(e.status(&id("nope"))),
    WorkflowError::NotFound("nope".into())
  );
  assert_eq!(
    err(e.result::<Orders>(&id("nope"))),
    WorkflowError::NotFound("nope".into())
  );
  assert_eq!(ok(e.resume(&id("nope"))), None);

  // `run_once` drives every claimable instance of the registered workflows.
  ok(e.start::<Orders>(id("other"), Count { n: 5 }));
  assert_eq!(ok(e.run_once()), 2);
  assert_eq!(ok(e.result::<Orders>(&id("other"))), Some(Count { n: 5 }));
  assert_eq!(ok(e.run_once()), 0);

  // An instance of a workflow this engine does not run: the lease is given back.
  ok(e.start::<Probe>(id("probe"), Count { n: 1 }));
  assert_eq!(
    err(e.resume(&id("probe"))),
    WorkflowError::UnknownWorkflow("probe".into())
  );
  assert_eq!(ok(store.get(&id("probe"))).unwrap().lease_owner, None);

  assert!(matches!(
    WorkflowEngine::new(store.clone())
      .register(Orders(Plan::Nothing))
      .and_then(|e| e.register(Orders(Plan::Nothing))),
    Err(WorkflowError::DuplicateWorkflow(_))
  ));
}

/// A workflow whose name is not a valid `WorkflowName`.
struct BadName;

impl Workflow for BadName {
  type Input = Count;
  type Output = Count;
  const NAME: &'static str = "bad name";

  fn run(&self, _: WorkflowContext, input: Count) -> Effect<Count, WorkflowError> {
    succeed(input)
  }
}

#[test]
fn invalid_names_are_rejected() {
  let store = MemoryWorkflowStore::new();
  assert!(matches!(
    WorkflowEngine::new(store.clone()).register(BadName),
    Err(WorkflowError::InvalidName(_))
  ));
  let e = WorkflowEngine::new(store);
  assert!(matches!(
    err(e.start::<BadName>(id("x"), Count { n: 1 })),
    WorkflowError::InvalidName(_)
  ));
}
