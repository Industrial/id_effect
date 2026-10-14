//! Forward steps with LIFO compensation (saga pattern).
//!
//! [`Saga::run`] returns an [`Effect`]: forward steps and compensations all run inside
//! the caller's interpreter, so a saga composes with other effects instead of blocking.

use crate::error::SagaError;
use id_effect::{Effect, box_future};
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

/// Ordered saga runner with automatic compensation on forward failure.
///
/// Steps are shared behind [`Arc`]s, so [`Saga::run`] can be called repeatedly; each call
/// builds a fresh effect.
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
  A: 'static,
  C: 'static,
  Err: 'static,
  R: 'static,
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
  /// [`SagaError::Compensate`].
  pub fn run(&self) -> Effect<(), SagaError<Err>, R> {
    let steps = self.steps.clone();
    Effect::new_async(move |env: &mut R| {
      box_future(async move {
        for (idx, step) in steps.iter().enumerate() {
          if let Err(err) = (step.forward)().run(env).await {
            compensate_reverse(&steps[..idx], env).await?;
            return Err(SagaError::Forward(err));
          }
        }
        Ok(())
      })
    })
  }

  /// Effect compensating the given step indices in reverse order (indices out of range are
  /// ignored).
  pub fn compensate_indices(&self, indices: &[usize]) -> Effect<(), SagaError<Err>, R> {
    let steps: Vec<_> = indices
      .iter()
      .filter_map(|&idx| self.steps.get(idx).cloned())
      .collect();
    Effect::new_async(move |env: &mut R| {
      box_future(async move { compensate_reverse(&steps, env).await })
    })
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

async fn compensate_reverse<A, C, Err, R>(
  steps: &[Arc<SagaStep<A, C, Err, R>>],
  env: &mut R,
) -> Result<(), SagaError<Err>>
where
  A: 'static,
  C: 'static,
  Err: 'static,
  R: 'static,
{
  for step in steps.iter().rev() {
    if let Some(comp) = &step.compensate {
      comp().run(env).await.map_err(SagaError::Compensate)?;
    }
  }
  Ok(())
}

#[cfg(test)]
mod saga_tests {
  use super::*;
  use id_effect::{fail, run_blocking, succeed};
  use std::sync::Mutex;

  type TestSaga = Saga<(), (), String, ()>;

  fn recorder(
    log: &Arc<Mutex<Vec<&'static str>>>,
    entry: &'static str,
  ) -> impl Fn() -> Effect<(), String, ()> + Send + Sync + 'static {
    let log = Arc::clone(log);
    move || {
      let log = Arc::clone(&log);
      Effect::new(move |_| {
        log.lock().unwrap().push(entry);
        Ok(())
      })
    }
  }

  #[test]
  fn step_name_is_preserved() {
    let step = SagaStep::<(), (), String, ()>::forward("reserve", || succeed(()));
    assert_eq!(step.name(), "reserve");
    let saga = TestSaga::new().step(step);
    assert_eq!(saga.len(), 1);
    assert!(!saga.is_empty());
  }

  #[test]
  fn empty_saga_succeeds() {
    let saga = TestSaga::new();
    assert!(saga.is_empty());
    run_blocking(saga.run(), ()).expect("empty run");
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
    let err = run_blocking(saga.run(), ()).expect_err("forward fail");
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
    let err = run_blocking(saga.run(), ()).expect_err("rollback fails");
    assert_eq!(err, SagaError::Compensate("undo-b broke".to_string()));
    assert_eq!(*log.lock().unwrap(), vec!["a", "b"]);
  }

  #[test]
  fn successful_run_is_repeatable() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let saga = TestSaga::new()
      .step(SagaStep::forward("a", recorder(&log, "a")))
      .step(SagaStep::forward("b", recorder(&log, "b")));
    run_blocking(saga.run(), ()).expect("first run");
    run_blocking(saga.run(), ()).expect("second run");
    assert_eq!(*log.lock().unwrap(), vec!["a", "b", "a", "b"]);
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
    run_blocking(saga.compensate_indices(&[0, 1, 7]), ()).expect("compensate");
    assert_eq!(*log.lock().unwrap(), vec!["undo-b", "undo-a"]);
  }

  #[test]
  fn steps_read_the_environment() {
    let saga = Saga::<(), (), String, u32>::new().step(SagaStep::forward("check", || {
      Effect::new(|budget: &mut u32| {
        if *budget >= 10 {
          Ok(())
        } else {
          Err(format!("budget {budget} too low"))
        }
      })
    }));
    assert!(run_blocking(saga.run(), 10).is_ok());
    assert_eq!(
      run_blocking(saga.run(), 3),
      Err(SagaError::Forward("budget 3 too low".to_string()))
    );
  }
}
