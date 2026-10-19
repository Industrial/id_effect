//! Engine failure paths on the in-memory store: a store wrapper injects the faults a database
//! produces (an outage, a lost lease, a refused park, a row gone between two reads) so the engine
//! is seen to stop, release, park or report each one the way its docs promise. `run` is driven
//! for a bounded time and interrupted.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use id_effect::{Cause, Effect, Exit, Schema, UtcDateTime, WallClock, fail, sleep, succeed};
use id_effect_jobs::MemoryJobQueue;
use id_effect_sql::SqlError;
use id_effect_workflow::{
  Claimed, Completion, EngineConfig, EngineId, Fenced, Finish, HistoryEvent, InstanceRecord, Lease,
  MemoryWorkflowStore, NewInstance, RunOutcome, SignalRecord, Workflow, WorkflowClient,
  WorkflowContext, WorkflowEngine, WorkflowError, WorkflowId, WorkflowName, WorkflowNameError,
  WorkflowStatus, WorkflowStore,
};

#[derive(Clone, Debug, PartialEq, Schema)]
struct Count {
  n: i64,
}

#[derive(Clone, Debug, PartialEq, Schema)]
struct Label {
  text: String,
}

/// How [`Faulty::get`] answers.
#[derive(Clone, Copy, Default)]
enum Get {
  #[default]
  Delegate,
  /// As if the row were deleted.
  Missing,
  /// The row without its output and error text.
  Stripped,
}

/// The faults to inject; each defaults to "delegate to the memory store".
#[derive(Default)]
struct Faults {
  get: std::sync::Mutex<Get>,
  /// `claim` fails with an outage while this is positive (counts down).
  claim_errors: AtomicUsize,
  append_error: AtomicBool,
  /// `renew` fails with an outage while this is positive (counts down).
  renew_errors: AtomicUsize,
  renew_lost: AtomicBool,
  renewals: AtomicUsize,
  finish_lost: AtomicBool,
  wait_lost: AtomicBool,
  release_lost: AtomicBool,
  release_error: AtomicBool,
}

fn outage<A: Send + 'static>() -> Effect<A, WorkflowError> {
  fail(WorkflowError::Sql(SqlError::NotConnected))
}

fn countdown(n: &AtomicUsize) -> bool {
  let mut current = n.load(Ordering::SeqCst);
  while current > 0 {
    match n.compare_exchange(current, current - 1, Ordering::SeqCst, Ordering::SeqCst) {
      Ok(_) => return true,
      Err(seen) => current = seen,
    }
  }
  false
}

/// A [`MemoryWorkflowStore`] with injected faults.
#[derive(Clone)]
struct Faulty {
  inner: MemoryWorkflowStore,
  faults: Arc<Faults>,
}

impl Faulty {
  fn new() -> Self {
    Self {
      inner: MemoryWorkflowStore::new(),
      faults: Arc::new(Faults::default()),
    }
  }
}

impl WorkflowStore for Faulty {
  fn create(&self, instance: NewInstance, now: UtcDateTime) -> Effect<bool, WorkflowError> {
    self.inner.create(instance, now)
  }

  fn get(&self, id: &WorkflowId) -> Effect<Option<InstanceRecord>, WorkflowError> {
    match *self.faults.get.lock().unwrap() {
      Get::Delegate => self.inner.get(id),
      Get::Missing => succeed(None),
      Get::Stripped => self.inner.get(id).map(|found| {
        found.map(|mut record| {
          record.output = None;
          record.error = None;
          record
        })
      }),
    }
  }

  fn acquire(
    &self,
    id: &WorkflowId,
    owner: &EngineId,
    now: UtcDateTime,
    expires_at: UtcDateTime,
  ) -> Effect<Option<Claimed>, WorkflowError> {
    self.inner.acquire(id, owner, now, expires_at)
  }

  fn claim(
    &self,
    name: &str,
    owner: &EngineId,
    limit: u16,
    now: UtcDateTime,
    expires_at: UtcDateTime,
  ) -> Effect<Vec<Claimed>, WorkflowError> {
    if countdown(&self.faults.claim_errors) {
      return outage();
    }
    self.inner.claim(name, owner, limit, now, expires_at)
  }

  fn history(&self, id: &WorkflowId) -> Effect<Vec<HistoryEvent>, WorkflowError> {
    self.inner.history(id)
  }

  fn append(
    &self,
    lease: &Lease,
    event: HistoryEvent,
    extend_to: UtcDateTime,
  ) -> Effect<Fenced, WorkflowError> {
    if self.faults.append_error.load(Ordering::SeqCst) {
      return outage();
    }
    self.inner.append(lease, event, extend_to)
  }

  fn renew(
    &self,
    lease: &Lease,
    now: UtcDateTime,
    expires_at: UtcDateTime,
  ) -> Effect<Fenced, WorkflowError> {
    self.faults.renewals.fetch_add(1, Ordering::SeqCst);
    if countdown(&self.faults.renew_errors) {
      return outage();
    }
    if self.faults.renew_lost.load(Ordering::SeqCst) {
      return succeed(Fenced::Lost);
    }
    self.inner.renew(lease, now, expires_at)
  }

  fn finish(
    &self,
    lease: &Lease,
    outcome: Finish,
    now: UtcDateTime,
  ) -> Effect<Fenced, WorkflowError> {
    if self.faults.finish_lost.load(Ordering::SeqCst) {
      return succeed(Fenced::Lost);
    }
    self.inner.finish(lease, outcome, now)
  }

  fn release(&self, lease: &Lease, now: UtcDateTime) -> Effect<Fenced, WorkflowError> {
    if self.faults.release_error.load(Ordering::SeqCst) {
      return outage();
    }
    if self.faults.release_lost.load(Ordering::SeqCst) {
      return succeed(Fenced::Lost);
    }
    self.inner.release(lease, now)
  }

  fn wait(&self, lease: &Lease, wakeups: i64, now: UtcDateTime) -> Effect<Fenced, WorkflowError> {
    if self.faults.wait_lost.load(Ordering::SeqCst) {
      return succeed(Fenced::Lost);
    }
    self.inner.wait(lease, wakeups, now)
  }

  fn completion(&self, id: &WorkflowId, seq: i64) -> Effect<Option<Completion>, WorkflowError> {
    self.inner.completion(id, seq)
  }

  fn complete(&self, id: &WorkflowId, completion: Completion) -> Effect<bool, WorkflowError> {
    self.inner.complete(id, completion)
  }

  fn signal(
    &self,
    id: &WorkflowId,
    name: &str,
    payload: serde_json::Value,
    now: UtcDateTime,
  ) -> Effect<Option<i64>, WorkflowError> {
    self.inner.signal(id, name, payload, now)
  }

  fn next_signal(
    &self,
    id: &WorkflowId,
    name: &str,
    after: i64,
  ) -> Effect<Option<SignalRecord>, WorkflowError> {
    self.inner.next_signal(id, name, after)
  }
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

fn engine(store: &Faulty, lease: Duration) -> WorkflowEngine {
  WorkflowEngine::new(store.clone())
    .with_config(EngineConfig {
      id: EngineId::new("faulty"),
      lease,
      poll_interval: Duration::from_millis(5),
    })
    .with_clock(clock())
}

/// One journaled step that doubles the input.
struct Double;

impl Workflow for Double {
  type Input = Count;
  type Output = Count;
  const NAME: &'static str = "double";

  fn run(&self, ctx: WorkflowContext, input: Count) -> Effect<Count, WorkflowError> {
    ctx.step("double", succeed::<_, String, _>(Count { n: input.n * 2 }))
  }
}

/// The same name as [`Double`] with another output schema: a reader whose code changed.
struct DoubleAsLabel;

impl Workflow for DoubleAsLabel {
  type Input = Count;
  type Output = Label;
  const NAME: &'static str = "double";

  fn run(&self, _: WorkflowContext, _: Count) -> Effect<Label, WorkflowError> {
    succeed(Label {
      text: "never run".into(),
    })
  }
}

/// A step that takes `.0` milliseconds of real time, longer than a short lease's heartbeat.
struct Slow(u64);

impl Workflow for Slow {
  type Input = Count;
  type Output = Count;
  const NAME: &'static str = "slow";

  fn run(&self, ctx: WorkflowContext, input: Count) -> Effect<Count, WorkflowError> {
    let work: Effect<Count, String> = sleep(Duration::from_millis(self.0)).map(move |()| input);
    ctx.step("work", work)
  }
}

/// Waits for signal `go`; with a client it first signals `other` to itself, so a wakeup
/// arrives during the run.
struct Waits(Option<WorkflowClient>);

impl Workflow for Waits {
  type Input = Count;
  type Output = Count;
  const NAME: &'static str = "waits";

  fn run(&self, ctx: WorkflowContext, _: Count) -> Effect<Count, WorkflowError> {
    match &self.0 {
      None => ctx.wait_signal::<Count>("go"),
      Some(client) => {
        let me = ctx.instance().clone();
        client
          .signal(&me, "other", Count { n: 0 })
          .flat_map(move |_| ctx.wait_signal::<Count>("go"))
      }
    }
  }
}

/// A durable timer: needs a job queue.
struct Sleeps;

impl Workflow for Sleeps {
  type Input = Count;
  type Output = Count;
  const NAME: &'static str = "sleeps";

  fn run(&self, ctx: WorkflowContext, input: Count) -> Effect<Count, WorkflowError> {
    ctx.sleep(Duration::from_secs(60)).map(move |()| input)
  }
}

/// Fails on purpose.
struct Refuses;

impl Workflow for Refuses {
  type Input = Count;
  type Output = Count;
  const NAME: &'static str = "refuses";

  fn run(&self, _: WorkflowContext, _: Count) -> Effect<Count, WorkflowError> {
    fail(WorkflowError::failed("refused"))
  }
}

#[test]
fn accessors_and_debug_describe_the_engine() {
  let store = Faulty::new();
  let e = engine(&store, Duration::from_secs(9))
    .register(Double)
    .unwrap();
  assert_eq!(e.id(), &EngineId::new("faulty"));
  assert_eq!(e.config().lease, Duration::from_secs(9));
  assert_eq!(e.config().poll_interval, Duration::from_millis(5));
  let debug = format!("{e:?}");
  assert!(debug.starts_with("WorkflowEngine"), "{debug}");
  assert!(debug.contains("\"double\""), "{debug}");
  assert!(debug.contains("job_queue: false"), "{debug}");

  // The engine reads and writes through the store it was given.
  ok(e.start::<Double>(id("d-1"), Count { n: 1 }));
  assert!(ok(e.store().get(&id("d-1"))).is_some());
  assert!(ok(store.inner.get(&id("d-1"))).is_some());

  let defaults = EngineConfig::default();
  assert_eq!(defaults.lease, Duration::from_secs(30));
  assert_eq!(defaults.poll_interval, Duration::from_millis(500));
  assert_ne!(defaults.id, EngineConfig::default().id, "random engine ids");
}

#[test]
fn a_store_outage_mid_run_frees_the_lease_and_decides_nothing() {
  let store = Faulty::new();
  let e = engine(&store, Duration::from_secs(30))
    .register(Double)
    .unwrap();
  let i = id("outage-1");
  ok(e.start::<Double>(i.clone(), Count { n: 3 }));
  store.faults.append_error.store(true, Ordering::SeqCst);
  assert_eq!(
    err(e.resume(&i)),
    WorkflowError::Sql(SqlError::NotConnected)
  );
  let record = ok(store.inner.get(&i)).unwrap();
  assert_eq!(record.status, WorkflowStatus::Running, "not failed");
  assert_eq!(record.lease_owner, None, "lease released at once");
  assert!(ok(store.inner.history(&i)).is_empty());

  // Back up: the instance resumes at once and completes.
  store.faults.append_error.store(false, Ordering::SeqCst);
  assert_eq!(ok(e.resume(&i)), Some(RunOutcome::Completed));
  assert_eq!(ok(e.result::<Double>(&i)), Some(Count { n: 6 }));
}

#[test]
fn a_timer_without_a_queue_or_with_a_queue_outage_frees_the_lease() {
  let store = Faulty::new();
  let bare = engine(&store, Duration::from_secs(30))
    .register(Sleeps)
    .unwrap();
  let i = id("sleeps-1");
  ok(bare.start::<Sleeps>(i.clone(), Count { n: 1 }));
  assert_eq!(err(bare.resume(&i)), WorkflowError::NoJobQueue);
  assert_eq!(ok(store.inner.get(&i)).unwrap().lease_owner, None);
  assert_eq!(ok(bare.status(&i)), WorkflowStatus::Running);

  let queue = MemoryJobQueue::new(clock());
  queue.fail_with(Some("queue down"));
  let queued = engine(&store, Duration::from_secs(30))
    .with_job_queue(queue.clone())
    .register(Sleeps)
    .unwrap();
  assert!(matches!(err(queued.resume(&i)), WorkflowError::Jobs(_)));
  let record = ok(store.inner.get(&i)).unwrap();
  assert_eq!(
    (record.status, record.lease_owner),
    (WorkflowStatus::Running, None)
  );

  // The queue recovers: the timer is scheduled and the run parks on it.
  queue.fail_with(None);
  assert_eq!(ok(queued.resume(&i)), Some(RunOutcome::Waiting));
  assert_eq!(queue.len(), 1, "one timer job");
  assert!(ok(store.inner.get(&i)).unwrap().waiting);
}

#[test]
fn a_rejected_renewal_stops_the_run_as_lease_lost() {
  let store = Faulty::new();
  store.faults.renew_lost.store(true, Ordering::SeqCst);
  let e = engine(&store, Duration::from_millis(30))
    .register(Slow(2_000))
    .unwrap();
  let i = id("slow-1");
  ok(e.start::<Slow>(i.clone(), Count { n: 1 }));
  let started = Instant::now();
  assert_eq!(ok(e.resume(&i)), Some(RunOutcome::LeaseLost));
  assert!(
    started.elapsed() < Duration::from_millis(1_500),
    "the heartbeat stopped the run before its step finished"
  );
  assert!(store.faults.renewals.load(Ordering::SeqCst) >= 1);
  assert!(
    ok(store.inner.history(&i)).is_empty(),
    "nothing written after the loss"
  );
  assert_eq!(ok(e.status(&i)), WorkflowStatus::Running);
}

#[test]
fn renewal_outages_are_retried_at_the_next_beat() {
  let store = Faulty::new();
  store.faults.renew_errors.store(2, Ordering::SeqCst);
  let e = engine(&store, Duration::from_millis(30))
    .register(Slow(150))
    .unwrap();
  let i = id("slow-2");
  ok(e.start::<Slow>(i.clone(), Count { n: 4 }));
  assert_eq!(ok(e.resume(&i)), Some(RunOutcome::Completed));
  assert!(
    store.faults.renewals.load(Ordering::SeqCst) >= 3,
    "two failed beats, then real renewals"
  );
  assert_eq!(ok(e.result::<Slow>(&i)), Some(Count { n: 4 }));
}

#[test]
fn a_finish_after_the_lease_moved_reports_lease_lost() {
  let store = Faulty::new();
  store.faults.finish_lost.store(true, Ordering::SeqCst);
  let e = engine(&store, Duration::from_secs(30))
    .register(Double)
    .unwrap()
    .register(Refuses)
    .unwrap();
  let (done, failed) = (id("finish-1"), id("finish-2"));
  ok(e.start::<Double>(done.clone(), Count { n: 1 }));
  ok(e.start::<Refuses>(failed.clone(), Count { n: 1 }));
  assert_eq!(ok(e.resume(&done)), Some(RunOutcome::LeaseLost));
  assert_eq!(ok(e.resume(&failed)), Some(RunOutcome::LeaseLost));
  assert_eq!(ok(e.status(&done)), WorkflowStatus::Running);
  assert_eq!(ok(e.status(&failed)), WorkflowStatus::Running);

  // The lost finishes left the leases in place; an engine whose clock is past their expiry
  // takes the instances over and finishes them.
  store.faults.finish_lost.store(false, Ordering::SeqCst);
  let refused = WorkflowError::failed("refused");
  let fresh = WorkflowEngine::new(store.clone())
    .with_config(EngineConfig {
      id: EngineId::new("later"),
      ..EngineConfig::default()
    })
    .with_clock(WallClock::anchored(
      Instant::now(),
      UtcDateTime::unsafe_make(1_800_000_000_000),
    ))
    .register(Double)
    .unwrap()
    .register(Refuses)
    .unwrap();
  assert_eq!(ok(fresh.resume(&done)), Some(RunOutcome::Completed));
  assert_eq!(
    ok(fresh.resume(&failed)),
    Some(RunOutcome::Failed(refused.clone()))
  );
  assert_eq!(
    err(fresh.result::<Refuses>(&failed)),
    WorkflowError::Failed(refused.to_string())
  );
}

#[test]
fn a_refused_park_releases_the_lease_or_reports_it_lost() {
  let store = Faulty::new();
  let e = engine(&store, Duration::from_secs(30))
    .register(Waits(None))
    .unwrap();

  // Parked normally: the instance waits and is not claimable.
  let parked = id("waits-1");
  ok(e.start::<Waits>(parked.clone(), Count { n: 0 }));
  assert_eq!(ok(e.resume(&parked)), Some(RunOutcome::Waiting));
  assert!(ok(store.inner.get(&parked)).unwrap().waiting);
  assert_eq!(ok(e.resume(&parked)), None, "parked: not claimable");

  // The park is refused and the lease freed instead: it can run again at once.
  store.faults.wait_lost.store(true, Ordering::SeqCst);
  let refused = id("waits-2");
  ok(e.start::<Waits>(refused.clone(), Count { n: 0 }));
  assert_eq!(ok(e.resume(&refused)), Some(RunOutcome::Waiting));
  let record = ok(store.inner.get(&refused)).unwrap();
  assert_eq!((record.waiting, record.lease_owner), (false, None));

  // Refused, and the release is fenced out too: the lease moved on.
  store.faults.release_lost.store(true, Ordering::SeqCst);
  assert_eq!(ok(e.resume(&refused)), Some(RunOutcome::LeaseLost));
}

#[test]
fn a_wakeup_during_the_run_refuses_the_park_so_it_runs_again() {
  let store = MemoryWorkflowStore::new();
  let c = clock();
  let client = WorkflowClient::new(store.clone()).with_clock(c.clone());
  let e = WorkflowEngine::new(store.clone())
    .with_config(EngineConfig {
      id: EngineId::new("wakes"),
      ..EngineConfig::default()
    })
    .with_clock(c)
    .register(Waits(Some(client)))
    .unwrap();
  let i = id("wakes-1");
  ok(e.start::<Waits>(i.clone(), Count { n: 0 }));
  assert_eq!(ok(e.resume(&i)), Some(RunOutcome::Waiting));
  let record = ok(store.get(&i)).unwrap();
  assert!(!record.waiting, "a signal arrived since the claim");
  assert_eq!(record.lease_owner, None);
  assert!(record.wakeups >= 1);

  assert!(ok(e.client().signal(&i, "go", Count { n: 5 })));
  assert_eq!(ok(e.resume(&i)), Some(RunOutcome::Completed));
  assert_eq!(ok(e.result::<Waits>(&i)), Some(Count { n: 5 }));
}

#[test]
fn start_and_result_report_rows_that_vanish_or_no_longer_decode() {
  let store = Faulty::new();
  let e = engine(&store, Duration::from_secs(30))
    .register(Double)
    .unwrap()
    .register(Refuses)
    .unwrap();
  let i = id("rows-1");
  ok(e.start::<Double>(i.clone(), Count { n: 2 }));
  assert_eq!(ok(e.resume(&i)), Some(RunOutcome::Completed));

  // The output is stored under the old schema: a reader with another output type fails.
  assert!(matches!(
    err(e.result::<DoubleAsLabel>(&i)),
    WorkflowError::Decode { what, .. } if what == "output"
  ));

  let f = id("rows-2");
  ok(e.start::<Refuses>(f.clone(), Count { n: 0 }));
  assert!(matches!(ok(e.resume(&f)), Some(RunOutcome::Failed(_))));

  // A completed row without an output, a failed row without its error text.
  *store.faults.get.lock().unwrap() = Get::Stripped;
  assert_eq!(
    err(e.result::<Double>(&i)),
    WorkflowError::Decode {
      what: "output".into(),
      message: "a completed instance has no output".into(),
    }
  );
  assert_eq!(
    err(e.result::<Refuses>(&f)),
    WorkflowError::Failed("unknown error".into())
  );

  // The insert conflicts but the row is gone by the time it is read back.
  *store.faults.get.lock().unwrap() = Get::Missing;
  assert_eq!(
    err(e.start::<Double>(i.clone(), Count { n: 2 })),
    WorkflowError::NotFound("rows-1".into())
  );
  assert_eq!(ok(e.instance(&i)), None);
}

#[test]
fn an_unknown_workflow_is_reported_even_when_the_release_fails() {
  let store = Faulty::new();
  let producer = engine(&store, Duration::from_secs(30));
  let i = id("foreign-1");
  ok(producer.start::<Double>(i.clone(), Count { n: 1 }));
  store.faults.release_error.store(true, Ordering::SeqCst);
  assert_eq!(
    err(producer.resume(&i)),
    WorkflowError::UnknownWorkflow("double".into())
  );
  assert_eq!(
    ok(store.inner.get(&i)).unwrap().lease_owner,
    Some(EngineId::new("faulty")),
    "best-effort release: the lease is left to expire"
  );
}

#[test]
fn run_retries_store_outages_and_drives_instances_until_interrupted() {
  let store = Faulty::new();
  store.faults.claim_errors.store(2, Ordering::SeqCst);
  let e = engine(&store, Duration::from_secs(30))
    .register(Double)
    .unwrap();
  for n in 1..=3 {
    ok(e.start::<Double>(id(&format!("run-{n}")), Count { n }));
  }
  let forever = e.run().map(|never| match never {});
  let bounded = forever.race_first(sleep(Duration::from_millis(300)));
  assert_eq!(ok(bounded), ());
  assert_eq!(
    store.faults.claim_errors.load(Ordering::SeqCst),
    0,
    "both outages were met and retried"
  );
  for n in 1..=3 {
    assert_eq!(
      ok(e.result::<Double>(&id(&format!("run-{n}")))),
      Some(Count { n: n * 2 })
    );
  }
}

/// The same name as [`Double`] with another input schema.
struct DoubleFromLabel;

impl Workflow for DoubleFromLabel {
  type Input = Label;
  type Output = Count;
  const NAME: &'static str = "double";

  fn run(&self, _: WorkflowContext, _: Label) -> Effect<Count, WorkflowError> {
    succeed(Count { n: 0 })
  }
}

#[test]
fn an_input_that_no_longer_decodes_fails_the_instance() {
  let store = Faulty::new();
  let producer = engine(&store, Duration::from_secs(30));
  let i = id("input-1");
  ok(producer.start::<Double>(i.clone(), Count { n: 1 }));
  let changed = engine(&store, Duration::from_secs(30))
    .register(DoubleFromLabel)
    .unwrap();
  match ok(changed.resume(&i)) {
    Some(RunOutcome::Failed(WorkflowError::Decode { what, .. })) => assert_eq!(what, "input"),
    other => panic!("expected an input decode failure, got {other:?}"),
  }
  assert_eq!(ok(changed.status(&i)), WorkflowStatus::Failed);
}

/// Notes what its context reports, journals a step, then waits for `go`.
#[derive(Clone, Default)]
struct Inspect {
  seen: Arc<std::sync::Mutex<Vec<(String, bool)>>>,
}

impl Workflow for Inspect {
  type Input = Count;
  type Output = Count;
  const NAME: &'static str = "inspect";

  fn run(&self, ctx: WorkflowContext, input: Count) -> Effect<Count, WorkflowError> {
    self
      .seen
      .lock()
      .unwrap()
      .push((format!("{ctx:?}"), ctx.is_replaying()));
    let next = ctx.clone();
    ctx
      .step("first", succeed::<_, String, _>(input))
      .flat_map(move |_| next.wait_signal::<Count>("go"))
  }
}

#[test]
fn the_context_reports_its_instance_and_whether_it_replays() {
  let store = MemoryWorkflowStore::new();
  let inspect = Inspect::default();
  let e = WorkflowEngine::new(store.clone())
    .with_clock(clock())
    .register(inspect.clone())
    .unwrap();
  let i = id("inspect-1");
  ok(e.start::<Inspect>(i.clone(), Count { n: 1 }));
  assert_eq!(ok(e.resume(&i)), Some(RunOutcome::Waiting));
  assert!(ok(e.client().signal(&i, "go", Count { n: 2 })));
  assert_eq!(ok(e.resume(&i)), Some(RunOutcome::Completed));

  let seen = inspect.seen.lock().unwrap().clone();
  assert_eq!(seen.len(), 2);
  assert!(!seen[0].1, "nothing journaled on the first run");
  assert!(seen[1].1, "the second run replays step `first`");
  assert!(seen[0].0.starts_with("WorkflowContext"), "{}", seen[0].0);
  assert!(seen[0].0.contains("inspect-1"), "{}", seen[0].0);
  assert!(seen[0].0.contains("history: 0"), "{}", seen[0].0);
  assert!(seen[1].0.contains("history: 1"), "{}", seen[1].0);
  assert!(format!("{store:?}").contains("instances: 1"));
}

/// A durable timer to an instant.
struct SleepsUntil;

impl Workflow for SleepsUntil {
  type Input = Count;
  type Output = Count;
  const NAME: &'static str = "sleeps-until";

  fn run(&self, ctx: WorkflowContext, input: Count) -> Effect<Count, WorkflowError> {
    ctx
      .sleep_until(UtcDateTime::unsafe_make(1_700_000_060_000))
      .map(move |()| input)
  }
}

#[test]
fn sleep_until_schedules_one_timer_and_parks() {
  let store = Faulty::new();
  let queue = MemoryJobQueue::new(clock());
  let e = engine(&store, Duration::from_secs(30))
    .with_job_queue(queue.clone())
    .register(SleepsUntil)
    .unwrap();
  let i = id("until-1");
  ok(e.start::<SleepsUntil>(i.clone(), Count { n: 1 }));
  assert_eq!(ok(e.resume(&i)), Some(RunOutcome::Waiting));
  assert_eq!(queue.len(), 1, "one timer job");
  assert!(ok(store.inner.get(&i)).unwrap().waiting);
}

#[test]
fn workflow_names_report_why_they_are_rejected() {
  let long = "x".repeat(WorkflowName::MAX_LEN + 1);
  let too_long = WorkflowName::new(long.as_str()).unwrap_err();
  assert_eq!(
    too_long,
    WorkflowNameError::TooLong {
      len: WorkflowName::MAX_LEN + 1
    }
  );
  assert_eq!(
    too_long.to_string(),
    format!(
      "workflow name is {} bytes, longer than {}",
      WorkflowName::MAX_LEN + 1,
      WorkflowName::MAX_LEN
    )
  );
  let name = WorkflowName::new("close-account").unwrap();
  assert_eq!(name.as_str(), "close-account");
  assert_eq!(format!("[{name:>15}]"), "[  close-account]");
  let as_error: &dyn std::error::Error = &WorkflowNameError::Empty;
  assert_eq!(as_error.to_string(), "workflow name is empty");
}
