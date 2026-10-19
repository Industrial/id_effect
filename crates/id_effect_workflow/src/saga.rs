//! Durable sagas (fault.workflows#3): compensations registered in a workflow body are journaled
//! and run as [`Activity`] calls, in reverse order, when the body fails, so a rollback survives
//! restarts like every other call of the instance. The in-memory saga of `id_effect_fsm`
//! (`id_effect_fsm::Saga`) stays the variant for effects that need no durability.

use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use id_effect::{Effect, fail, succeed, suspend};

use crate::WorkflowName;
use crate::activity::Activity;
use crate::error::WorkflowError;
use crate::workflow::WorkflowContext;

/// The step a compensation is registered under: `compensate:<activity name>`.
const COMPENSATE: &str = "compensate:";

/// One registered compensation: the call of its activity.
type Compensation = Box<dyn FnOnce(&WorkflowContext) -> Effect<(), WorkflowError> + Send>;

/// The compensations of one [`WorkflowContext::saga`] body, registered with
/// [`compensate`](Self::compensate) or [`activity`](Self::activity). Cloning shares them.
///
/// A registration is journaled as the step `compensate:<NAME>` holding the compensation's
/// input, so the replay of an instance registers the same compensations with the same inputs
/// (and a body that no longer does fails with [`WorkflowError::NonDeterministic`]).
#[derive(Clone)]
pub struct Saga {
  ctx: WorkflowContext,
  compensations: Arc<Mutex<Vec<Compensation>>>,
}

impl fmt::Debug for Saga {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("Saga")
      .field("instance", self.ctx.instance())
      .field("compensations", &self.len())
      .finish_non_exhaustive()
  }
}

impl Saga {
  fn pending(&self) -> MutexGuard<'_, Vec<Compensation>> {
    self
      .compensations
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
  }

  /// The number of registered compensations that have not run.
  pub fn len(&self) -> usize {
    self.pending().len()
  }

  /// Whether no compensation is registered.
  pub fn is_empty(&self) -> bool {
    self.len() == 0
  }

  /// Registers activity `C` with `input` as a compensation: when the saga's body fails, it
  /// runs (after every compensation registered later) as [`WorkflowContext::activity`] does,
  /// at least once and retried by [`C::retry`](Activity::retry).
  ///
  /// The registration is journaled (a step); fails with [`WorkflowError::InvalidName`] when
  /// [`C::NAME`](Activity::NAME) is not a valid [`WorkflowName`].
  pub fn compensate<C: Activity>(&self, input: C::Input) -> Effect<(), WorkflowError> {
    if let Err(e) = WorkflowName::new(C::NAME) {
      return fail(e.into());
    }
    let saga = self.clone();
    self
      .ctx
      .step(
        format!("{COMPENSATE}{}", C::NAME),
        succeed::<_, String, _>(input),
      )
      .map(move |input| {
        let call: Compensation = Box::new(move |ctx| ctx.activity::<C>(input).map(|_| ()));
        saga.pending().push(call);
      })
  }

  /// Runs activity `A` with `input` and, once it succeeded, registers activity `C` as its
  /// compensation, with the input `undo` makes of `A`'s output. A final failure of `A`
  /// registers nothing: register a compensation before the call with
  /// [`compensate`](Self::compensate) when a failed attempt may leave something to undo.
  pub fn activity<A, C, F>(&self, input: A::Input, undo: F) -> Effect<A::Output, WorkflowError>
  where
    A: Activity,
    C: Activity,
    F: FnOnce(&A::Output) -> C::Input + Send + 'static,
  {
    let saga = self.clone();
    self.ctx.activity::<A>(input).flat_map(move |output| {
      let input = undo(&output);
      saga.compensate::<C>(input).map(move |()| output)
    })
  }

  /// Runs the registered compensations, last registered first, one after another; the errors
  /// of those that failed for good. Stops at an error that stops the run (a park, a lost lease,
  /// a divergence, a store failure), which the next run resumes from.
  fn unwind(&self) -> Effect<Vec<WorkflowError>, WorkflowError> {
    let pending = std::mem::take(&mut *self.pending());
    unwind(self.ctx.clone(), pending, Vec::new())
  }
}

fn unwind(
  ctx: WorkflowContext,
  mut pending: Vec<Compensation>,
  mut failed: Vec<WorkflowError>,
) -> Effect<Vec<WorkflowError>, WorkflowError> {
  let Some(call) = pending.pop() else {
    return succeed(failed);
  };
  call(&ctx)
    .map(|()| None)
    .catch(|e| {
      if fails_instance(&e) {
        succeed(Some(e))
      } else {
        fail(e)
      }
    })
    .flat_map(move |error| {
      failed.extend(error);
      unwind(ctx, pending, failed)
    })
}

/// Whether `e` fails the instance (the engine's rule): not a park, a lost lease, a divergence
/// (nothing can be journaled after it), nor a store or job-queue failure (the instance resumes
/// once they are back).
fn fails_instance(e: &WorkflowError) -> bool {
  !matches!(
    e,
    WorkflowError::Waiting
      | WorkflowError::LeaseLost
      | WorkflowError::NonDeterministic { .. }
      | WorkflowError::Sql(_)
      | WorkflowError::Jobs(_)
      | WorkflowError::NoJobQueue
  )
}

/// `error` once the compensations ran: unchanged when they all succeeded, otherwise a
/// [`WorkflowError::Failed`] naming it and every compensation that failed.
fn after_compensation(error: WorkflowError, failed: Vec<WorkflowError>) -> WorkflowError {
  if failed.is_empty() {
    return error;
  }
  let mut message = error.to_string();
  for e in failed {
    message.push_str("; compensation failed: ");
    message.push_str(&e.to_string());
  }
  WorkflowError::Failed(message)
}

impl WorkflowContext {
  /// Runs `body` as a durable saga: the compensations it registers on its [`Saga`]
  /// ([`Saga::compensate`], [`Saga::activity`]) run when it fails, in reverse order of
  /// registration, as journaled activity calls.
  ///
  /// - **Success**: the output; the compensations are dropped.
  /// - **A failure that fails the instance** (a step or activity failure,
  ///   [`WorkflowError::failed`], a decode error): every compensation runs, last registered
  ///   first, each [at least once](Activity) and retried by its schedule; then the saga fails
  ///   with the body's error, or, when compensations failed for good (the rest still run), with
  ///   [`WorkflowError::Failed`] naming the body's error and theirs.
  /// - **A park, a lost lease, a divergence or a store failure** compensates nothing: the run
  ///   stops, and the next run replays the body (registering the same compensations from the
  ///   journal) and goes on where this one stopped. A rollback interrupted by a crash or a park
  ///   resumes the same way: the body's failure and the finished compensations replay from the
  ///   history and the rest run.
  ///
  /// Defects are not compensated (as in `id_effect_fsm::Saga`): the run dies and resumes.
  pub fn saga<A, F>(&self, body: F) -> Effect<A, WorkflowError>
  where
    A: Send + 'static,
    F: FnOnce(Saga) -> Effect<A, WorkflowError> + Send + 'static,
  {
    let saga = Saga {
      ctx: self.clone(),
      compensations: Arc::new(Mutex::new(Vec::new())),
    };
    suspend(move || {
      let rollback = saga.clone();
      body(saga).catch(move |error| {
        if !fails_instance(&error) {
          return fail(error);
        }
        rollback
          .unwind()
          .flat_map(move |failed| fail(after_compensation(error, failed)))
      })
    })
  }
}

/// Sagas on the memory store and queue; the restart and crash cases run on PostgreSQL
/// (`tests/saga_pg.rs`).
#[cfg(test)]
mod tests {
  use std::time::Instant;

  use id_effect::{Cause, Exit, UtcDateTime, WallClock, hlist, sync};
  use id_effect_jobs::{
    JobFailure, JobQueueEnv, JobQueueService, JobRegistry, MemoryJobQueue, Worker, WorkerConfig,
  };

  use super::*;
  use crate::{
    ACTIVITY_QUEUE, ActivityContext, EngineConfig, EngineId, EventKind, MemoryWorkflowStore,
    Workflow, WorkflowEngine, WorkflowId, WorkflowStatus, WorkflowStore,
  };

  #[derive(Clone, Debug, PartialEq, id_effect::Schema)]
  struct Leg {
    n: i64,
  }

  type Log = Arc<Mutex<Vec<String>>>;

  fn entry(log: &Log, line: String) -> Effect<(), JobFailure> {
    let log = Arc::clone(log);
    sync(move || log.lock().unwrap().push(line))
  }

  /// Books leg `n`.
  struct Book(Log);

  impl Activity for Book {
    type Input = Leg;
    type Output = Leg;
    const NAME: &'static str = "book";

    fn run(&self, _: ActivityContext, leg: Leg) -> Effect<Leg, JobFailure> {
      entry(&self.0, format!("book {}", leg.n)).map(move |()| leg)
    }
  }

  /// Cancels leg `n`; with the flag set, leg 2 cannot be cancelled.
  struct Cancel(Log, bool);

  impl Activity for Cancel {
    type Input = Leg;
    type Output = Leg;
    const NAME: &'static str = "cancel";
    const MAX_ATTEMPTS: u16 = 1;

    fn run(&self, _: ActivityContext, leg: Leg) -> Effect<Leg, JobFailure> {
      if self.1 && leg.n == 2 {
        return fail(JobFailure::discard("leg 2 already flown"));
      }
      entry(&self.0, format!("cancel {}", leg.n)).map(move |()| leg)
    }
  }

  /// Pays `n`; declines a negative amount.
  struct Pay(Log);

  impl Activity for Pay {
    type Input = Leg;
    type Output = Leg;
    const NAME: &'static str = "pay";
    const MAX_ATTEMPTS: u16 = 1;

    fn run(&self, _: ActivityContext, leg: Leg) -> Effect<Leg, JobFailure> {
      if leg.n < 0 {
        return fail(JobFailure::discard("declined"));
      }
      entry(&self.0, format!("pay {}", leg.n)).map(move |()| leg)
    }
  }

  /// Books legs 1 and 2 (each compensated by `cancel`), then pays the input.
  struct Trip;

  impl Workflow for Trip {
    type Input = Leg;
    type Output = Leg;
    const NAME: &'static str = "trip";

    fn run(&self, ctx: WorkflowContext, amount: Leg) -> Effect<Leg, WorkflowError> {
      let pay = ctx.clone();
      ctx.saga(move |saga| {
        let second = saga.clone();
        saga
          .activity::<Book, Cancel, _>(Leg { n: 1 }, Leg::clone)
          .flat_map(move |_| second.activity::<Book, Cancel, _>(Leg { n: 2 }, Leg::clone))
          .flat_map(move |_| pay.activity::<Pay>(amount))
      })
    }
  }

  /// Registers a compensation whose name is invalid after one that is valid, then fails.
  struct BadName;

  struct Invalid;

  impl Activity for Invalid {
    type Input = Leg;
    type Output = Leg;
    const NAME: &'static str = "not valid";

    fn run(&self, _: ActivityContext, leg: Leg) -> Effect<Leg, JobFailure> {
      succeed(leg)
    }
  }

  impl Workflow for BadName {
    type Input = Leg;
    type Output = Leg;
    const NAME: &'static str = "bad-name";

    fn run(&self, ctx: WorkflowContext, leg: Leg) -> Effect<Leg, WorkflowError> {
      ctx.saga(move |saga| {
        let (next, debug) = (saga.clone(), saga.clone());
        assert!(saga.is_empty());
        saga
          .compensate::<Cancel>(leg.clone())
          .flat_map(move |()| {
            assert_eq!(debug.len(), 1);
            assert!(format!("{debug:?}").starts_with("Saga"));
            next.compensate::<Invalid>(leg)
          })
          .map(|()| Leg { n: 0 })
      })
    }
  }

  fn ok<A: Send + 'static, E: fmt::Debug + Send + 'static>(eff: Effect<A, E>) -> A {
    match eff.block_on() {
      Exit::Success(a) => a,
      Exit::Failure(c) => panic!("unexpected failure: {c:?}"),
    }
  }

  struct Setup {
    store: MemoryWorkflowStore,
    queue: MemoryJobQueue,
    engine: WorkflowEngine,
    registry: JobRegistry<JobQueueEnv>,
    clock: WallClock,
    log: Log,
  }

  /// `stuck`: leg 2 cannot be cancelled.
  fn setup(stuck: bool) -> Setup {
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
      .register(Trip)
      .unwrap()
      .register(BadName)
      .unwrap();
    let log = Log::default();
    let registry = JobRegistry::new();
    let registry = engine
      .register_activity(registry, Book(Arc::clone(&log)))
      .unwrap();
    let registry = engine
      .register_activity(registry, Cancel(Arc::clone(&log), stuck))
      .unwrap();
    let registry = engine
      .register_activity(registry, Pay(Arc::clone(&log)))
      .unwrap();
    Setup {
      store,
      queue,
      engine,
      registry,
      clock,
      log,
    }
  }

  impl Setup {
    /// Resumes `id` and drains the activity queue until it finishes.
    fn finish(&self, id: &WorkflowId) -> WorkflowStatus {
      for _ in 0..50 {
        ok(self.engine.resume(id));
        let status = ok(self.engine.status(id));
        if status.is_terminal() {
          return status;
        }
        let config = WorkerConfig {
          clock: self.clock.clone(),
          ..WorkerConfig::default()
        };
        let worker = Worker::new(self.registry.clone())
          .queue(ACTIVITY_QUEUE, 2)
          .with_config(config);
        let env = hlist![JobQueueService::new(self.queue.clone())];
        ok(worker.run_until_idle().provide_all(env));
      }
      panic!("{id} did not finish");
    }

    fn log(&self) -> Vec<String> {
      self.log.lock().unwrap().clone()
    }

    fn history(&self, id: &WorkflowId) -> Vec<(EventKind, String)> {
      ok(self.store.history(id))
        .into_iter()
        .map(|e| (e.kind, e.name))
        .collect()
    }

    fn error(&self, id: &WorkflowId) -> String {
      match self.engine.result::<Trip>(id).block_on() {
        Exit::Failure(Cause::Fail(WorkflowError::Failed(message))) => message,
        other => panic!("expected the recorded failure, got {other:?}"),
      }
    }
  }

  fn id(s: &str) -> WorkflowId {
    WorkflowId::new(s).unwrap()
  }

  fn step(name: &str) -> (EventKind, String) {
    (EventKind::Step, name.to_owned())
  }

  fn activity(name: &str) -> (EventKind, String) {
    (EventKind::Activity, name.to_owned())
  }

  #[test]
  fn a_failed_body_compensates_in_reverse_then_fails() {
    let s = setup(false);
    let i = id("trip-1");
    ok(s.engine.start::<Trip>(i.clone(), Leg { n: -1 }));
    assert_eq!(s.finish(&i), WorkflowStatus::Failed);
    assert_eq!(s.log(), ["book 1", "book 2", "cancel 2", "cancel 1"]);
    assert_eq!(
      s.history(&i),
      [
        activity("book"),
        step("compensate:cancel"),
        activity("book"),
        step("compensate:cancel"),
        activity("pay"),
        activity("cancel"),
        activity("cancel"),
      ]
    );
    assert_eq!(s.error(&i), "activity `pay` failed: declined");
    let journaled = ok(s.store.history(&i));
    assert_eq!(
      journaled[3].payload,
      serde_json::json!({ "ok": { "n": 2 } })
    );
  }

  #[test]
  fn a_completed_body_runs_no_compensation() {
    let s = setup(false);
    let i = id("trip-2");
    ok(s.engine.start::<Trip>(i.clone(), Leg { n: 30 }));
    assert_eq!(s.finish(&i), WorkflowStatus::Completed);
    assert_eq!(ok(s.engine.result::<Trip>(&i)), Some(Leg { n: 30 }));
    assert_eq!(s.log(), ["book 1", "book 2", "pay 30"]);
    assert_eq!(s.history(&i).len(), 5);
  }

  #[test]
  fn a_failed_compensation_does_not_stop_the_rest() {
    let s = setup(true);
    let i = id("trip-3");
    ok(s.engine.start::<Trip>(i.clone(), Leg { n: -1 }));
    assert_eq!(s.finish(&i), WorkflowStatus::Failed);
    assert_eq!(s.log(), ["book 1", "book 2", "cancel 1"]);
    assert_eq!(
      s.error(&i),
      "workflow failed: activity `pay` failed: declined; compensation failed: activity `cancel` \
       failed: leg 2 already flown"
    );
  }

  #[test]
  fn an_invalid_compensation_name_fails_after_compensating() {
    let s = setup(false);
    let i = id("bad-1");
    ok(s.engine.start::<BadName>(i.clone(), Leg { n: 5 }));
    assert_eq!(s.finish(&i), WorkflowStatus::Failed);
    assert_eq!(s.log(), ["cancel 5"]);
    assert_eq!(
      s.history(&i),
      [step("compensate:cancel"), activity("cancel")]
    );
    match ok(s.engine.instance(&i)).and_then(|r| r.error) {
      Some(error) => assert!(error.starts_with("invalid workflow name"), "{error}"),
      None => panic!("no recorded error"),
    }
  }

  #[test]
  fn only_errors_that_fail_the_instance_compensate() {
    assert!(fails_instance(&WorkflowError::failed("x")));
    assert!(fails_instance(&WorkflowError::Step {
      step: "s".into(),
      message: "m".into()
    }));
    for stop in [
      WorkflowError::Waiting,
      WorkflowError::LeaseLost,
      WorkflowError::NoJobQueue,
      WorkflowError::NonDeterministic {
        seq: 1,
        history: "h".into(),
        code: "c".into(),
      },
    ] {
      assert!(!fails_instance(&stop), "{stop}");
    }
    assert_eq!(
      after_compensation(WorkflowError::LeaseLost, Vec::new()),
      WorkflowError::LeaseLost
    );
    // A park mid-rollback stops it (`run_once` would resume it); here: no queue at all.
    let s = setup(false);
    let bare = WorkflowEngine::new(s.store.clone())
      .with_clock(s.clock.clone())
      .register(Trip)
      .unwrap();
    let i = id("trip-4");
    ok(bare.start::<Trip>(i.clone(), Leg { n: -1 }));
    match bare.resume(&i).block_on() {
      Exit::Failure(Cause::Fail(WorkflowError::NoJobQueue)) => {}
      other => panic!("expected NoJobQueue, got {other:?}"),
    }
    assert_eq!(ok(bare.status(&i)), WorkflowStatus::Running);
    assert!(s.history(&i).is_empty() && s.log().is_empty());
  }
}
