//! Activities (release 0.11.0 decision D6): the side effects of a workflow that run as
//! `id_effect_jobs` jobs on a named queue, on workers of any node, retried by a core
//! [`Schedule`], and delivered to the instance once per idempotency key `(instance, seq)`.

use std::fmt;
use std::marker::PhantomData;
use std::sync::Arc;
use std::time::Duration;

use id_effect::schema::{ParseError, Schema, Unknown};
use id_effect::{Effect, HList, HasSchema, Schedule, WallClock, fail, succeed, suspend};
use id_effect_jobs::{
  Job, JobContext, JobFailure, JobOptions, JobRegistry, NewJob, UniqueKey, default_retry,
  retry_delay,
};

use crate::WorkflowName;
use crate::error::WorkflowError;
use crate::history::{Completion, EventKind, WorkflowStore};
use crate::workflow::{Journaled, Position, WorkflowContext, WorkflowId, decode, encode, ready};

/// The queue activities are enqueued on unless [`Activity::QUEUE`] names another. A worker must
/// serve it (`Worker::queue(ACTIVITY_QUEUE, n)`).
pub const ACTIVITY_QUEUE: &str = "workflow.activities";

/// An activity: a side effect a workflow runs through [`WorkflowContext::activity`] as a job of
/// the `id_effect_jobs` queue, so any worker on any node runs it, its failures are retried with
/// [`retry`](Self::retry) up to [`MAX_ATTEMPTS`](Self::MAX_ATTEMPTS) attempts, and its result is
/// journaled in the instance's history.
///
/// Delivery is at least once: a worker that dies mid-run leaves the job to be rescued and run
/// again, so [`run`](Self::run) must be idempotent; [`ActivityContext::idempotency_key`] is a
/// stable key to hand to the systems it calls. The first result recorded for a key wins; a
/// later one (a rescued duplicate) is ignored.
///
/// Register the implementation on the workers' [`JobRegistry`] with
/// [`WorkflowEngine::register_activity`](crate::WorkflowEngine::register_activity); the
/// workflow side only needs the type. [`NAME`](Self::NAME) is both the job name and the
/// history name (a valid [`WorkflowName`]): keep it stable, and distinct from the registry's
/// other jobs.
pub trait Activity: Send + Sync + 'static {
  /// What the workflow passes it.
  type Input: Journaled;
  /// What it returns to the workflow.
  type Output: Journaled;

  /// The activity's stable name.
  const NAME: &'static str;
  /// The job queue it runs on (default [`ACTIVITY_QUEUE`]).
  const QUEUE: &'static str = ACTIVITY_QUEUE;
  /// How many attempts before its failure is final (default 20; at least 1).
  const MAX_ATTEMPTS: u16 = 20;
  /// The time limit of one attempt; an attempt that exceeds it fails and is retried (default
  /// none).
  const TIMEOUT: Option<Duration> = None;

  /// The backoff between attempts, stepped once per failed attempt (default
  /// [`default_retry`]). A schedule that is done makes the failure final before the attempts
  /// run out.
  fn retry() -> Schedule<(), JobFailure> {
    default_retry()
  }

  /// One attempt. [`JobFailure::Retry`] tries again after the [`retry`](Self::retry) delay;
  /// [`JobFailure::Discard`], the last attempt's failure, or a done schedule is final: the
  /// failure is journaled and the workflow's call fails with [`WorkflowError::Activity`].
  fn run(&self, ctx: ActivityContext, input: Self::Input) -> Effect<Self::Output, JobFailure>;
}

/// What one attempt of an activity knows about itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActivityContext {
  /// The workflow instance that scheduled it.
  pub instance: WorkflowId,
  /// The history position it completes.
  pub seq: i64,
  /// This attempt's number, from 1.
  pub attempt: u16,
  /// The most attempts it gets.
  pub max_attempts: u16,
}

impl ActivityContext {
  /// `<instance>#<seq>`: the same on every attempt and every replay of this call, and the job's
  /// unique key, so it can deduplicate the activity's effect in the systems it calls.
  pub fn idempotency_key(&self) -> String {
    idempotency_key(&self.instance, self.seq)
  }
}

/// The idempotency key of history position `seq` of `instance` (activities and timers).
pub(crate) fn idempotency_key(instance: &WorkflowId, seq: i64) -> String {
  format!("{instance}#{seq}")
}

/// The job of one scheduled activity call: the instance, the position and the encoded input.
pub(crate) struct ActivityTask<T> {
  instance: String,
  seq: i64,
  input: Unknown,
  activity: PhantomData<fn() -> T>,
}

impl<T> ActivityTask<T> {
  fn into_unknown(self) -> Unknown {
    Unknown::Object(
      [
        ("instance".to_owned(), Unknown::String(self.instance)),
        ("seq".to_owned(), Unknown::I64(self.seq)),
        ("input".to_owned(), self.input),
      ]
      .into_iter()
      .collect(),
    )
  }

  fn from_unknown(u: &Unknown) -> Result<Self, ParseError> {
    let Unknown::Object(fields) = u else {
      return Err(ParseError::new("", "expected object"));
    };
    let Some(Unknown::String(instance)) = fields.get("instance") else {
      return Err(ParseError::new("instance", "expected string"));
    };
    let Some(Unknown::I64(seq)) = fields.get("seq") else {
      return Err(ParseError::new("seq", "expected i64"));
    };
    Ok(Self {
      instance: instance.clone(),
      seq: *seq,
      input: fields.get("input").cloned().unwrap_or(Unknown::Null),
      activity: PhantomData,
    })
  }
}

impl<T: 'static> HasSchema for ActivityTask<T> {
  type A = Self;
  type I = Unknown;
  type E = ();

  fn schema() -> Schema<Self, Unknown, ()> {
    Schema::make(
      |u: Unknown| Self::from_unknown(&u),
      Self::into_unknown,
      Self::from_unknown,
    )
  }
}

impl<T: Activity> Job for ActivityTask<T> {
  const NAME: &'static str = T::NAME;
  const QUEUE: &'static str = T::QUEUE;

  fn max_attempts(&self) -> u16 {
    T::MAX_ATTEMPTS.max(1)
  }

  fn retry() -> Schedule<(), JobFailure> {
    T::retry()
  }
}

impl WorkflowContext {
  /// Runs activity `T` with `input` as a job and returns its journaled result.
  ///
  /// On the first run the call enqueues the job on [`T::QUEUE`](Activity::QUEUE) with the
  /// unique key `<instance>#<seq>` (so a call scheduled again after a crash or by a replay
  /// inserts nothing) and parks the run ([`WorkflowError::Waiting`]); a worker runs it, records
  /// its result for that key, and wakes the instance, whose next run journals the result here.
  /// On replay the journaled result is returned and nothing is enqueued. A final failure is
  /// journaled too and returned as [`WorkflowError::Activity`] on this run and every replay.
  /// Needs an engine with a job queue ([`WorkflowError::NoJobQueue`] otherwise).
  pub fn activity<T: Activity>(&self, input: T::Input) -> Effect<T::Output, WorkflowError> {
    if let Err(e) = WorkflowName::new(T::NAME) {
      return fail(e.into());
    }
    let ctx = self.clone();
    let input = <T::Input as HasSchema>::schema().encode(input);
    suspend(move || match ctx.position(EventKind::Activity, T::NAME) {
      Err(e) => fail(e),
      Ok(Position::Replayed(payload)) => ready(activity_result::<T>(&payload)),
      Ok(Position::Fresh(seq)) => {
        let schedule = schedule::<T>(&ctx, seq, input);
        ctx
          .delivered(seq, EventKind::Activity, T::NAME.to_owned(), schedule)
          .flat_map(|payload| ready(activity_result::<T>(&payload)))
      }
    })
  }
}

/// Enqueues the job of activity call `seq` (a duplicate of its key inserts nothing).
fn schedule<T: Activity>(
  ctx: &WorkflowContext,
  seq: i64,
  input: Unknown,
) -> Effect<(), WorkflowError> {
  let jobs = match ctx.jobs() {
    Ok(jobs) => jobs,
    Err(e) => return fail(e),
  };
  let key = idempotency_key(ctx.instance(), seq);
  let task = ActivityTask::<T> {
    instance: ctx.instance().as_str().to_owned(),
    seq,
    input,
    activity: PhantomData,
  };
  let opts = JobOptions::new().unique(UniqueKey::until_pruned(key));
  jobs
    .enqueue_raw(NewJob::of(task, opts))
    .map(|_| ())
    .map_err(WorkflowError::Jobs)
}

/// A journaled activity payload as the call's result.
fn activity_result<T: Activity>(payload: &serde_json::Value) -> Result<T::Output, WorkflowError> {
  if let Some(value) = payload.get("ok") {
    return decode(&format!("activity {}", T::NAME), value);
  }
  match payload.get("err").and_then(serde_json::Value::as_str) {
    Some(message) => Err(WorkflowError::Activity {
      activity: T::NAME.to_owned(),
      message: message.to_owned(),
    }),
    None => Err(WorkflowError::Decode {
      what: format!("activity {}", T::NAME),
      message: format!("malformed journal payload {payload}"),
    }),
  }
}

/// What a worker needs to deliver an activity's result.
struct Deliver {
  store: Arc<dyn WorkflowStore>,
  clock: WallClock,
  instance: WorkflowId,
  seq: i64,
  name: &'static str,
}

impl Deliver {
  /// Records `payload` for the call (ignored when a result is already recorded) and wakes the
  /// instance; a store failure retries the job.
  fn send(&self, payload: serde_json::Value) -> Effect<(), JobFailure> {
    let (store, instance, seq, name) = (
      Arc::clone(&self.store),
      self.instance.clone(),
      self.seq,
      self.name,
    );
    self.clock.now().flat_map(move |now| {
      let completion = Completion {
        seq,
        kind: EventKind::Activity,
        name: name.to_owned(),
        payload,
        completed_at: now,
      };
      store
        .complete(&instance, completion)
        .map(|_| ())
        .map_err(JobFailure::retry)
    })
  }

  /// Records the final failure `message` and ends the job `dead` with it.
  fn give_up(&self, message: String) -> Effect<(), JobFailure> {
    self
      .send(serde_json::json!({ "err": message }))
      .then(fail(JobFailure::Discard(message)))
  }
}

/// Adds the handler of activity `T` to `registry`: it runs `activity` and delivers the result
/// to the instance through `store`.
pub(crate) fn register<T: Activity, R>(
  registry: JobRegistry<R>,
  activity: T,
  store: Arc<dyn WorkflowStore>,
  clock: WallClock,
) -> Result<JobRegistry<R>, WorkflowError>
where
  R: HList + Send + Sync + 'static,
{
  WorkflowName::new(T::NAME)?;
  let activity = Arc::new(activity);
  registry
    .register::<ActivityTask<T>, JobFailure>(move |task, job| {
      let deliver = Deliver {
        store: Arc::clone(&store),
        clock: clock.clone(),
        instance: WorkflowId::stored(task.instance.clone()),
        seq: task.seq,
        name: T::NAME,
      };
      attempt(Arc::clone(&activity), deliver, task, job).narrow()
    })
    .map_err(WorkflowError::Jobs)
}

/// One attempt of an activity job: run it, deliver a success or a final failure, or fail the
/// attempt so the queue retries it.
fn attempt<T: Activity>(
  activity: Arc<T>,
  deliver: Deliver,
  task: ActivityTask<T>,
  job: JobContext,
) -> Effect<(), JobFailure> {
  let input = match <T::Input as HasSchema>::schema().decode_unknown_all(&task.input) {
    Ok(input) => input,
    Err(errors) => {
      return deliver.give_up(format!(
        "the input of activity `{}` does not decode: {errors}",
        T::NAME
      ));
    }
  };
  let ctx = ActivityContext {
    instance: deliver.instance.clone(),
    seq: deliver.seq,
    attempt: job.attempt,
    max_attempts: job.max_attempts,
  };
  let run = suspend(move || activity.run(ctx, input));
  let run = match T::TIMEOUT {
    None => run,
    Some(limit) => run.timeout(limit).flat_map(move |done| match done {
      Some(output) => succeed(output),
      None => fail(JobFailure::Retry(format!("timed out after {limit:?}"))),
    }),
  };
  run
    .map(Ok)
    .catch(|failure| succeed(Err(failure)))
    .flat_map(move |result| match result {
      Ok(output) => deliver.send(serde_json::json!({ "ok": encode(output) })),
      Err(failure) if is_final::<T>(&failure, &job) => {
        deliver.give_up(failure.message().to_owned())
      }
      Err(failure) => fail(failure),
    })
}

/// Whether `failure` of attempt `job` is the last: a discard, the last attempt, or a done retry
/// schedule.
fn is_final<T: Activity>(failure: &JobFailure, job: &JobContext) -> bool {
  failure.is_discard()
    || job.attempt >= job.max_attempts
    || retry_delay(&T::retry(), failure, job.attempt).is_none()
}

impl<T> fmt::Debug for ActivityTask<T> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("ActivityTask")
      .field("instance", &self.instance)
      .field("seq", &self.seq)
      .finish_non_exhaustive()
  }
}

/// The engine, the job queue and the worker in memory: activities, timers and signals end to
/// end, and the edges the PostgreSQL tests do not reach.
#[cfg(test)]
mod tests {
  use std::time::Instant;

  use id_effect::{Cause, Exit, UtcDateTime, hlist, sleep};
  use id_effect_jobs::{
    DrainReport, JobQueueEnv, JobQueueService, MemoryJobQueue, Queue, Worker, WorkerConfig,
  };

  use super::*;
  use crate::history::HistoryEvent;
  use crate::{
    EngineConfig, EngineId, MemoryWorkflowStore, RunOutcome, TIMER_QUEUE, Workflow, WorkflowEngine,
    WorkflowStatus,
  };

  #[derive(Clone, Debug, PartialEq, id_effect::Schema)]
  struct Count {
    n: i64,
  }

  struct Double;

  impl Activity for Double {
    type Input = Count;
    type Output = Count;
    const NAME: &'static str = "double";

    fn run(&self, ctx: ActivityContext, input: Count) -> Effect<Count, JobFailure> {
      assert_eq!(
        ctx.idempotency_key(),
        format!("{}#{}", ctx.instance, ctx.seq)
      );
      succeed(Count { n: input.n * 2 })
    }
  }

  /// Slower than its time limit.
  struct Slow;

  impl Activity for Slow {
    type Input = Count;
    type Output = Count;
    const NAME: &'static str = "slow";
    const TIMEOUT: Option<Duration> = Some(Duration::from_millis(5));

    fn run(&self, _: ActivityContext, input: Count) -> Effect<Count, JobFailure> {
      sleep(Duration::from_millis(200)).map(move |()| input)
    }
  }

  /// Doubles, sleeps an hour, then adds the `go` signal.
  struct Flow;

  impl Workflow for Flow {
    type Input = Count;
    type Output = Count;
    const NAME: &'static str = "flow";

    fn run(&self, ctx: WorkflowContext, input: Count) -> Effect<Count, WorkflowError> {
      let (c2, c3) = (ctx.clone(), ctx.clone());
      ctx.activity::<Double>(input).flat_map(move |doubled| {
        c2.sleep(Duration::from_secs(3600)).flat_map(move |()| {
          c3.wait_signal::<Count>("go").map(move |go| Count {
            n: doubled.n + go.n,
          })
        })
      })
    }
  }

  /// Swallows every error of its activity, then tries a step.
  struct Swallow;

  impl Workflow for Swallow {
    type Input = Count;
    type Output = Count;
    const NAME: &'static str = "swallow";

    fn run(&self, ctx: WorkflowContext, input: Count) -> Effect<Count, WorkflowError> {
      let next = ctx.clone();
      ctx
        .activity::<Double>(input)
        .catch(|_| succeed(Count { n: 0 }))
        .flat_map(move |c| next.step("after", succeed::<_, String, _>(c)))
    }
  }

  fn ok<A: Send + 'static, E: fmt::Debug + Send + 'static>(eff: Effect<A, E>) -> A {
    match eff.block_on() {
      Exit::Success(a) => a,
      Exit::Failure(c) => panic!("unexpected failure: {c:?}"),
    }
  }

  fn failure<A: fmt::Debug + Send + 'static, E: fmt::Debug + Send + 'static>(
    eff: Effect<A, E>,
  ) -> E {
    match eff.block_on() {
      Exit::Failure(Cause::Fail(e)) => e,
      other => panic!("expected a typed failure, got {other:?}"),
    }
  }

  struct Setup {
    store: MemoryWorkflowStore,
    queue: MemoryJobQueue,
    engine: WorkflowEngine,
    registry: JobRegistry<JobQueueEnv>,
    clock: WallClock,
  }

  fn setup() -> Setup {
    let clock = WallClock::anchored(Instant::now(), UtcDateTime::unsafe_make(1_700_000_000_000));
    let (store, queue) = (
      MemoryWorkflowStore::new(),
      MemoryJobQueue::new(clock.clone()),
    );
    let engine = WorkflowEngine::new(store.clone())
      .with_config(EngineConfig {
        id: EngineId::new("e"),
        ..EngineConfig::default()
      })
      .with_clock(clock.clone())
      .with_job_queue(queue.clone())
      .register(Flow)
      .unwrap()
      .register(Swallow)
      .unwrap();
    let registry = engine
      .register_activity(JobRegistry::new(), Double)
      .unwrap();
    let registry = engine.register_activity(registry, Slow).unwrap();
    let registry = engine.register_timers(registry).unwrap();
    Setup {
      store,
      queue,
      engine,
      registry,
      clock,
    }
  }

  impl Setup {
    fn drain(&self) -> DrainReport {
      let config = WorkerConfig {
        clock: self.clock.clone(),
        ..WorkerConfig::default()
      };
      let worker = Worker::new(self.registry.clone())
        .queue(ACTIVITY_QUEUE, 2)
        .queue(TIMER_QUEUE, 2)
        .with_config(config);
      let env = hlist![JobQueueService::new(self.queue.clone())];
      ok(worker.run_until_idle().provide_all(env))
    }
  }

  fn id(s: &str) -> WorkflowId {
    WorkflowId::new(s).unwrap()
  }

  #[test]
  fn memory_engine_runs_activities_timers_and_signals() {
    let s = setup();
    let i = id("flow-1");
    ok(s.engine.start::<Flow>(i.clone(), Count { n: 4 }));
    assert_eq!(ok(s.engine.resume(&i)), Some(RunOutcome::Waiting));
    assert_eq!(ok(s.engine.resume(&i)), None, "parked");
    assert_eq!(s.drain().succeeded, 1, "the activity");
    assert_eq!(
      ok(s.engine.resume(&i)),
      Some(RunOutcome::Waiting),
      "the timer"
    );
    assert_eq!(s.drain().runs(), 0, "not due");
    s.clock.jump(Duration::from_secs(3601));
    assert_eq!(s.drain().succeeded, 1, "the timer fired");
    assert_eq!(
      ok(s.engine.resume(&i)),
      Some(RunOutcome::Waiting),
      "the signal"
    );
    assert!(ok(s.engine.client().signal(&i, "go", Count { n: 1 })));
    assert_eq!(ok(s.engine.resume(&i)), Some(RunOutcome::Completed));
    assert_eq!(ok(s.engine.result::<Flow>(&i)), Some(Count { n: 9 }));
    let kinds: Vec<EventKind> = ok(s.store.history(&i)).iter().map(|e| e.kind).collect();
    assert_eq!(
      kinds,
      [EventKind::Activity, EventKind::Timer, EventKind::Signal]
    );
    assert!(format!("{:?}", s.engine).contains("job_queue: true"));
    assert!(format!("{:?}", s.engine.client()).starts_with("WorkflowClient"));
  }

  #[test]
  fn a_swallowed_wait_still_parks_and_a_mismatched_delivery_fails() {
    let s = setup();
    let i = id("swallow-1");
    ok(s.engine.start::<Swallow>(i.clone(), Count { n: 1 }));
    assert_eq!(ok(s.engine.resume(&i)), Some(RunOutcome::Waiting));
    assert!(
      ok(s.store.history(&i)).is_empty(),
      "the step after the wait never ran"
    );

    // A completion of another kind at the activity's position: the code changed.
    let wrong = Completion {
      seq: 1,
      kind: EventKind::Timer,
      name: "timer".into(),
      payload: serde_json::json!({ "due": "x" }),
      completed_at: UtcDateTime::unsafe_make(0),
    };
    assert!(ok(s.store.complete(&i, wrong)));
    match ok(s.engine.resume(&i)) {
      Some(RunOutcome::Failed(WorkflowError::NonDeterministic { seq, history, code })) => {
        assert_eq!((seq, history.as_str()), (1, "a delivered timer"));
        assert_eq!(code, "activity double");
      }
      other => panic!("expected a nondeterministic failure, got {other:?}"),
    }
    assert_eq!(ok(s.engine.status(&i)), WorkflowStatus::Failed);
  }

  #[test]
  fn activity_attempts_give_up_on_bad_input_and_retry_timeouts() {
    let s = setup();
    let i = id("edge-1");
    ok(s.engine.start::<Swallow>(i.clone(), Count { n: 1 }));
    let deliver = |seq| Deliver {
      store: Arc::clone(s.engine.store()),
      clock: s.clock.clone(),
      instance: i.clone(),
      seq,
      name: "slow",
    };
    let job = |attempt| JobContext {
      id: id_effect_jobs::JobId(1),
      name: "slow".into(),
      queue: Queue::new(ACTIVITY_QUEUE),
      attempt,
      max_attempts: 3,
      inserted_at: UtcDateTime::unsafe_make(0),
    };
    let task = |seq, input| ActivityTask::<Slow> {
      instance: i.as_str().to_owned(),
      seq,
      input,
      activity: PhantomData,
    };

    // An input that does not decode: final at once, journaled as the failure.
    let bad = attempt(Arc::new(Slow), deliver(1), task(1, Unknown::Null), job(1));
    let JobFailure::Discard(message) = failure(bad) else {
      panic!("expected a discard");
    };
    assert!(
      message.starts_with("the input of activity `slow` does not decode"),
      "{message}"
    );
    let delivered = ok(s.store.completion(&i, 1)).unwrap();
    assert_eq!(delivered.payload, serde_json::json!({ "err": message }));

    // A timeout retries, and gives up on the last attempt.
    let input = || Count::schema().encode(Count { n: 2 });
    let timed_out = attempt(Arc::new(Slow), deliver(2), task(2, input()), job(1));
    assert_eq!(
      failure(timed_out),
      JobFailure::Retry("timed out after 5ms".into())
    );
    assert_eq!(ok(s.store.completion(&i, 2)), None);
    let last = attempt(Arc::new(Slow), deliver(2), task(2, input()), job(3));
    assert!(failure(last).is_discard());
    assert!(ok(s.store.completion(&i, 2)).is_some());
  }

  #[test]
  fn tasks_and_payloads_decode_or_report() {
    let task = ActivityTask::<Double> {
      instance: "i".into(),
      seq: 3,
      input: Unknown::I64(1),
      activity: PhantomData,
    };
    let schema = ActivityTask::<Double>::schema();
    let back = schema.decode(schema.encode(task)).unwrap();
    assert_eq!((back.instance.as_str(), back.seq), ("i", 3));
    assert!(format!("{back:?}").contains("ActivityTask"));
    assert_eq!(back.max_attempts(), 20);
    assert_eq!(<ActivityTask<Double> as Job>::QUEUE, ACTIVITY_QUEUE);
    for (bad, path) in [
      (Unknown::Null, ""),
      (Unknown::Object(Default::default()), "instance"),
      (
        Unknown::Object([("instance".to_owned(), Unknown::String("i".into()))].into()),
        "seq",
      ),
    ] {
      assert_eq!(schema.decode_unknown(&bad).unwrap_err().path, path);
    }

    assert_eq!(
      activity_result::<Double>(&serde_json::json!({ "err": "no" })),
      Err(WorkflowError::Activity {
        activity: "double".into(),
        message: "no".into()
      })
    );
    assert!(matches!(
      activity_result::<Double>(&serde_json::json!(1)),
      Err(WorkflowError::Decode { .. })
    ));
    assert!(matches!(
      crate::timer::decode_due(&serde_json::json!({ "due": 1 })),
      Err(WorkflowError::Decode { .. })
    ));
    assert_eq!(HistoryEvent::describe(EventKind::Signal, "go"), "signal go");
    let ctx = ActivityContext {
      instance: id("i"),
      seq: 2,
      attempt: 1,
      max_attempts: 1,
    };
    assert_eq!(ctx.idempotency_key(), "i#2");
    let failure = JobFailure::retry("x");
    let job = JobContext {
      id: id_effect_jobs::JobId(1),
      name: "double".into(),
      queue: Queue::new(ACTIVITY_QUEUE),
      attempt: 1,
      max_attempts: 20,
      inserted_at: UtcDateTime::unsafe_make(0),
    };
    assert!(!is_final::<Double>(&failure, &job));
    assert!(is_final::<Double>(&JobFailure::discard("x"), &job));
  }

  #[test]
  fn a_caught_divergence_still_fails_the_instance() {
    let s = setup();
    let i = id("swallow-2");
    ok(s.engine.start::<Swallow>(i.clone(), Count { n: 1 }));
    // History from other code: a step where the body now calls an activity.
    let (zero, far) = (
      UtcDateTime::unsafe_make(0),
      UtcDateTime::unsafe_make(4_000_000_000_000),
    );
    let lease = ok(s.store.acquire(&i, &EngineId::new("old"), zero, far))
      .unwrap()
      .lease;
    let event = HistoryEvent {
      seq: 1,
      kind: EventKind::Step,
      name: "old".into(),
      payload: serde_json::json!({ "ok": { "n": 1 } }),
      recorded_at: UtcDateTime::unsafe_make(0),
    };
    assert!(ok(s.store.append(&lease, event, far)).is_applied());
    assert!(ok(s.store.release(&lease, zero)).is_applied());
    // Swallow catches the NonDeterministic error and calls a step: the step fails with it too,
    // nothing is journaled at seq 2, and the instance fails.
    match ok(s.engine.resume(&i)) {
      Some(RunOutcome::Failed(WorkflowError::NonDeterministic { seq: 1, .. })) => {}
      other => panic!("expected the divergence, got {other:?}"),
    }
    assert_eq!(ok(s.store.history(&i)).len(), 1);
  }

  #[test]
  fn run_once_drives_parked_instances_once_they_wake() {
    let s = setup();
    let ids: Vec<WorkflowId> = (0..3).map(|n| id(&format!("flow-{n}"))).collect();
    for i in &ids {
      ok(s.engine.start::<Flow>(i.clone(), Count { n: 1 }));
    }
    assert_eq!(ok(s.engine.run_once()), 3);
    assert_eq!(ok(s.engine.run_once()), 0, "all parked");
    assert_eq!(s.drain().succeeded, 3);
    assert_eq!(ok(s.engine.run_once()), 3, "woken by their activities");
    s.clock.jump(Duration::from_secs(3601));
    assert_eq!(s.drain().succeeded, 3);
    assert_eq!(ok(s.engine.run_once()), 3, "woken by their timers");
    for i in &ids {
      assert!(ok(s.engine.client().signal(i, "go", Count { n: 0 })));
    }
    assert_eq!(ok(s.engine.run_once()), 3);
    for i in &ids {
      assert_eq!(ok(s.engine.result::<Flow>(i)), Some(Count { n: 2 }));
    }
  }

  #[test]
  fn names_are_checked_and_signals_need_a_valid_payload() {
    let s = setup();
    struct Bad;
    impl Activity for Bad {
      type Input = Count;
      type Output = Count;
      const NAME: &'static str = "bad name";
      fn run(&self, _: ActivityContext, input: Count) -> Effect<Count, JobFailure> {
        succeed(input)
      }
    }
    assert!(matches!(
      s.engine
        .register_activity(JobRegistry::<JobQueueEnv>::new(), Bad),
      Err(WorkflowError::InvalidName(_))
    ));
    assert!(matches!(
      s.engine.register_activity(s.registry.clone(), Double),
      Err(WorkflowError::Jobs(_))
    ));
    assert!(matches!(
      s.engine.register_timers(s.registry.clone()),
      Err(WorkflowError::Jobs(_))
    ));
  }
}
