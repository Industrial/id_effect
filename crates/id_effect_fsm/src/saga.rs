//! Forward steps with LIFO compensation (saga pattern).
//!
//! [`Saga::run`] returns an [`Effect`]: forward steps and compensations are sequenced with
//! `flat_map`/`catch` on the caller's fiber, so a saga composes with other effects instead of
//! blocking.

use crate::error::SagaError;
use id_effect::next::{Effect, fail, succeed};
use std::sync::Arc;

type ForwardFn<A, Err, R> = Box<dyn Fn() -> Effect<A, Err, R> + Send + Sync>;
type CompensateFn<C, Err, R> = Box<dyn Fn() -> Effect<C, Err, R> + Send + Sync>;

/// One forward effect factory plus an optional compensating effect factory.
pub struct SagaStep<A: 'static, C: 'static, Err: 'static, R: 'static> {
  name: String,
  forward: ForwardFn<A, Err, R>,
  compensate: Option<CompensateFn<C, Err, R>>,
}

impl<A, C, Err, R> SagaStep<A, C, Err, R>
where
  A: 'static,
  C: 'static,
  Err: 'static,
  R: 'static,
{
  /// Forward-only step (no compensation).
  pub fn forward<F>(name: impl Into<String>, make: F) -> Self
  where
    F: Fn() -> Effect<A, Err, R> + Send + Sync + 'static,
  {
    Self {
      name: name.into(),
      forward: Box::new(make),
      compensate: None,
    }
  }

  /// Forward step with a compensating effect invoked on rollback.
  pub fn with_compensate<F, G>(name: impl Into<String>, forward: F, compensate: G) -> Self
  where
    F: Fn() -> Effect<A, Err, R> + Send + Sync + 'static,
    G: Fn() -> Effect<C, Err, R> + Send + Sync + 'static,
  {
    Self {
      name: name.into(),
      forward: Box::new(forward),
      compensate: Some(Box::new(compensate)),
    }
  }

  /// Step name (logging / diagnostics).
  pub fn name(&self) -> &str {
    &self.name
  }
}

type Steps<A, C, Err, R> = Arc<[Arc<SagaStep<A, C, Err, R>>]>;

/// Ordered saga runner with automatic compensation on forward failure.
///
/// The steps are [`id_effect::next::Effect`] factories over the environment `R`. Steps are
/// shared behind [`Arc`]s, so [`Saga::run`] can be called repeatedly; each call builds a fresh
/// effect.
pub struct Saga<A: 'static, C: 'static, Err: 'static, R: 'static> {
  steps: Vec<Arc<SagaStep<A, C, Err, R>>>,
}

impl<A, C, Err, R> Default for Saga<A, C, Err, R>
where
  A: 'static,
  C: 'static,
  Err: 'static,
  R: 'static,
{
  fn default() -> Self {
    Self { steps: Vec::new() }
  }
}

impl<A, C, Err, R> Saga<A, C, Err, R>
where
  A: Send + 'static,
  C: Send + 'static,
  Err: Send + 'static,
  R: Send + Sync + 'static,
{
  /// Empty saga.
  pub fn new() -> Self {
    Self::default()
  }

  /// Appends a step (builder style).
  pub fn step(mut self, step: SagaStep<A, C, Err, R>) -> Self {
    self.steps.push(Arc::new(step));
    self
  }

  /// Effect running all forward steps in order. On the first forward failure, completed
  /// steps are compensated in reverse order and the effect fails with
  /// [`SagaError::Forward`]; a failing compensation aborts the rollback with
  /// [`SagaError::Compensate`]. Defects and interrupts are not compensated (they pass
  /// through, as in Effect.ts).
  pub fn run(&self) -> Effect<(), SagaError<Err>, R> {
    forward_from(self.steps.iter().cloned().collect(), 0)
  }

  /// Effect compensating the given step indices in reverse order (indices out of range are
  /// ignored).
  pub fn compensate_indices(&self, indices: &[usize]) -> Effect<(), SagaError<Err>, R> {
    compensate_reverse(indices.iter().filter_map(|&idx| self.steps.get(idx)))
  }

  /// Number of registered steps.
  pub fn len(&self) -> usize {
    self.steps.len()
  }

  /// Whether the saga has no steps.
  pub fn is_empty(&self) -> bool {
    self.steps.is_empty()
  }
}

/// Runs forward step `idx` and the ones after it; on a typed failure, compensates `..idx`.
fn forward_from<A, C, Err, R>(
  steps: Steps<A, C, Err, R>,
  idx: usize,
) -> Effect<(), SagaError<Err>, R>
where
  A: Send + 'static,
  C: Send + 'static,
  Err: Send + 'static,
  R: Send + Sync + 'static,
{
  let Some(step) = steps.get(idx) else {
    return succeed(());
  };
  let forward = (step.forward)();
  let done = Arc::clone(&steps);
  forward
    .map(|_| ())
    .catch(move |err| compensate_reverse(done[..idx].iter()).then(fail(SagaError::Forward(err))))
    .flat_map(move |()| forward_from(steps, idx + 1))
}

/// Compensates `steps` from last to first (steps without a compensation are skipped).
fn compensate_reverse<'a, A, C, Err, R>(
  steps: impl DoubleEndedIterator<Item = &'a Arc<SagaStep<A, C, Err, R>>>,
) -> Effect<(), SagaError<Err>, R>
where
  A: Send + 'static,
  C: Send + 'static,
  Err: Send + 'static,
  R: Send + Sync + 'static,
{
  steps
    .rev()
    .filter(|step| step.compensate.is_some())
    .fold(succeed(()), |acc, step| {
      let step = Arc::clone(step);
      acc.flat_map(move |()| match &step.compensate {
        Some(comp) => comp().map(|_| ()).map_err(SagaError::Compensate),
        None => succeed(()),
      })
    })
}

#[cfg(test)]
mod saga_tests {
  use super::*;
  use crate::test_support::run;
  use id_effect::next::{HCons, HNil, need, sync};
  use std::sync::Mutex;

  type TestSaga = Saga<(), (), String, HNil>;

  fn recorder(
    log: &Arc<Mutex<Vec<&'static str>>>,
    entry: &'static str,
  ) -> impl Fn() -> Effect<(), String, HNil> + Send + Sync + 'static {
    let log = Arc::clone(log);
    move || {
      let log = Arc::clone(&log);
      sync(move || log.lock().unwrap().push(entry))
    }
  }

  #[test]
  fn step_name_is_preserved() {
    let step = SagaStep::<(), (), String, HNil>::forward("reserve", || succeed(()));
    assert_eq!(step.name(), "reserve");
    let saga = TestSaga::new().step(step);
    assert_eq!(saga.len(), 1);
    assert!(!saga.is_empty());
  }

  #[test]
  fn empty_saga_succeeds() {
    let saga = TestSaga::new();
    assert!(saga.is_empty());
    run(saga.run()).expect("empty run");
  }

  #[test]
  fn forward_failure_compensates_completed_steps_in_reverse() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let saga = TestSaga::new()
      .step(SagaStep::with_compensate(
        "reserve",
        recorder(&log, "reserve"),
        recorder(&log, "release"),
      ))
      .step(SagaStep::with_compensate(
        "hold",
        recorder(&log, "hold"),
        recorder(&log, "unhold"),
      ))
      .step(SagaStep::with_compensate(
        "charge",
        || fail("declined".to_string()),
        recorder(&log, "never"),
      ));
    let err = run(saga.run()).expect_err("forward fail");
    assert_eq!(err, SagaError::Forward("declined".to_string()));
    assert_eq!(
      *log.lock().unwrap(),
      vec!["reserve", "hold", "unhold", "release"]
    );
  }

  #[test]
  fn failing_compensation_aborts_rollback() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let saga = TestSaga::new()
      .step(SagaStep::with_compensate(
        "a",
        recorder(&log, "a"),
        recorder(&log, "undo-a"),
      ))
      .step(SagaStep::with_compensate("b", recorder(&log, "b"), || {
        fail("undo-b broke".to_string())
      }))
      .step(SagaStep::forward("c", || fail("c broke".to_string())));
    let err = run(saga.run()).expect_err("rollback fails");
    assert_eq!(err, SagaError::Compensate("undo-b broke".to_string()));
    assert_eq!(*log.lock().unwrap(), vec!["a", "b"]);
  }

  #[test]
  fn successful_run_is_repeatable() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let saga = TestSaga::new()
      .step(SagaStep::forward("a", recorder(&log, "a")))
      .step(SagaStep::forward("b", recorder(&log, "b")));
    run(saga.run()).expect("first run");
    run(saga.run()).expect("second run");
    assert_eq!(*log.lock().unwrap(), vec!["a", "b", "a", "b"]);
  }

  #[test]
  fn a_defect_is_not_compensated() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let saga = TestSaga::new()
      .step(SagaStep::with_compensate(
        "a",
        recorder(&log, "a"),
        recorder(&log, "undo-a"),
      ))
      .step(SagaStep::forward("b", || id_effect::next::die("boom")));
    let exit = saga.run().block_on();
    assert!(matches!(
      exit,
      id_effect::Exit::Failure(id_effect::Cause::Die(_))
    ));
    assert_eq!(*log.lock().unwrap(), vec!["a"]);
  }

  #[test]
  fn compensate_indices_runs_in_reverse_and_skips_unknown() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let saga = TestSaga::new()
      .step(SagaStep::with_compensate(
        "a",
        || succeed(()),
        recorder(&log, "undo-a"),
      ))
      .step(SagaStep::with_compensate(
        "b",
        || succeed(()),
        recorder(&log, "undo-b"),
      ));
    run(saga.compensate_indices(&[0, 1, 7])).expect("compensate");
    assert_eq!(*log.lock().unwrap(), vec!["undo-b", "undo-a"]);
  }

  struct Budget(u32);

  #[test]
  fn steps_read_the_environment() {
    let saga =
      Saga::<(), (), String, HCons<Budget, HNil>>::new().step(SagaStep::forward("check", || {
        need::<Budget>().flat_map(|budget| {
          if budget.0 >= 10 {
            succeed(())
          } else {
            fail(format!("budget {} too low", budget.0))
          }
        })
      }));
    assert!(run(saga.run().provide(Budget(10))).is_ok());
    assert_eq!(
      run(saga.run().provide(Budget(3))),
      Err(SagaError::Forward("budget 3 too low".to_string()))
    );
  }
}
