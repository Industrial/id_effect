//! Forward steps with LIFO compensation (saga pattern).

use crate::error::SagaError;
use id_effect::{Effect, run_blocking};

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
pub struct Saga<A: 'static, C: 'static, Err: 'static, R: 'static> {
  steps: Vec<SagaStep<A, C, Err, R>>,
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
  R: Clone + 'static,
{
  /// Empty saga.
  pub fn new() -> Self {
    Self::default()
  }

  /// Appends a step (builder style).
  pub fn step(mut self, step: SagaStep<A, C, Err, R>) -> Self {
    self.steps.push(step);
    self
  }

  /// Runs all forward steps via `run_blocking`. On failure, compensates completed steps in
  /// reverse order.
  pub fn run(&self, env: R) -> Result<(), SagaError<Err>> {
    let mut completed: Vec<usize> = Vec::new();
    for (idx, step) in self.steps.iter().enumerate() {
      match run_blocking((step.forward)(), env.clone()) {
        Ok(_) => completed.push(idx),
        Err(err) => {
          self.compensate_indices(&completed, &env)?;
          return Err(SagaError::Forward(err));
        }
      }
    }
    Ok(())
  }

  /// Compensates the given step indices in reverse order.
  pub fn compensate_indices(&self, indices: &[usize], env: &R) -> Result<(), SagaError<Err>> {
    for &idx in indices.iter().rev() {
      if let Some(comp) = &self.steps[idx].compensate {
        run_blocking(comp(), env.clone()).map_err(SagaError::Compensate)?;
      }
    }
    Ok(())
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
