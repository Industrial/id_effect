//! Resilience pipelines — compose the policies of this crate into one reusable value with
//! telemetry.
//!
//! A [`Pipeline`] is built once (`Pipeline::new(name)` plus the policies it needs) and applied
//! to any number of calls with [`Pipeline::execute`]. Whatever subset is configured, the
//! policies always nest in the same order, outermost first:
//!
//! | Order | Policy | Rejection / failure |
//! |-------|--------|---------------------|
//! | 1 | [`Fallback`] | recovers from the failures it handles |
//! | 2 | retry ([`Schedule`]) | re-runs layers 3–6 while the schedule continues and the error is [transient](PipelineError::is_transient) |
//! | 3 | [`RateLimiter`] | [`PipelineError::RateLimited`] (one token per attempt) |
//! | 4 | [`Bulkhead`] | [`PipelineError::BulkheadFull`] |
//! | 5 | [`CircuitBreaker`] | [`PipelineError::CircuitOpen`]; counts failures and timeouts |
//! | 6 | timeout | [`PipelineError::Timeout`] (per attempt) |
//! | 7 | the call | [`PipelineError::Failed`] |
//!
//! Every attempt, success, failure and fallback is reported as a [`PipelineEvent`] to the
//! listeners added with [`Pipeline::on_event`] and, with [`Pipeline::metrics`], counted in an
//! `id_effect` [`Frequency`] metric tagged `pipeline=<name>` (exported with the rest of the
//! metric registry, for example to Prometheus). All delays — timeout, backoff, breaker cooldown,
//! token refill — are on the fiber's executor clock, so virtual time on a
//! [`TestExecutor`](id_effect::TestExecutor).
//!
//! ```
//! use std::time::Duration;
//!
//! use id_effect::{Effect, Exit, Schedule, fail, succeed};
//! use id_effect_resilience::{CircuitBreaker, Fallback, Pipeline};
//!
//! let pipeline: Pipeline<&str, &str> = Pipeline::new("profile")
//!   .timeout(Duration::from_secs(2))
//!   .retry(Schedule::recurs(2))
//!   .circuit_breaker(&CircuitBreaker::new(5, Duration::from_secs(30)))
//!   .fallback(Fallback::value("cached profile"));
//!
//! assert_eq!(pipeline.execute(|| succeed("fresh")).block_on(), Exit::Success("fresh"));
//! // Three failed attempts, then the fallback answers.
//! let down = pipeline.execute(|| fail("503"));
//! assert_eq!(down.block_on(), Exit::Success("cached profile"));
//! ```

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use id_effect::{Effect, Frequency, HNil, Metric, Or, Schedule, fail, succeed, suspend, sync};

use crate::bulkhead::Bulkhead;
use crate::circuit_breaker::CircuitBreaker;
use crate::fallback::Fallback;
use crate::rate_limiter::RateLimiter;

// ── errors and events ───────────────────────────────────────────────────────────────────────

/// Why a [`Pipeline`] call failed.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum PipelineError<E> {
  /// The call itself failed with `E`.
  Failed(E),
  /// An attempt did not finish within the pipeline's timeout.
  Timeout,
  /// The circuit breaker is open.
  CircuitOpen,
  /// The rate limiter had no token.
  RateLimited,
  /// The bulkhead was full.
  BulkheadFull,
}

impl<E> PipelineError<E> {
  /// The error without its payload.
  pub fn kind(&self) -> PipelineErrorKind {
    match self {
      Self::Failed(_) => PipelineErrorKind::Failed,
      Self::Timeout => PipelineErrorKind::Timeout,
      Self::CircuitOpen => PipelineErrorKind::CircuitOpen,
      Self::RateLimited => PipelineErrorKind::RateLimited,
      Self::BulkheadFull => PipelineErrorKind::BulkheadFull,
    }
  }

  /// `true` for the errors a retry may cure (`Failed` and `Timeout`); the pipeline's retry
  /// stops at any other.
  pub fn is_transient(&self) -> bool {
    matches!(self, Self::Failed(_) | Self::Timeout)
  }

  /// The call's own error, if that is what this is.
  pub fn into_failed(self) -> Option<E> {
    match self {
      Self::Failed(e) => Some(e),
      _ => None,
    }
  }
}

impl<E: fmt::Display> fmt::Display for PipelineError<E> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Failed(e) => e.fmt(f),
      other => f.write_str(other.kind().label()),
    }
  }
}

impl<E: fmt::Debug + fmt::Display> std::error::Error for PipelineError<E> {}

/// [`PipelineError`] without its payload, for telemetry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PipelineErrorKind {
  /// [`PipelineError::Failed`].
  Failed,
  /// [`PipelineError::Timeout`].
  Timeout,
  /// [`PipelineError::CircuitOpen`].
  CircuitOpen,
  /// [`PipelineError::RateLimited`].
  RateLimited,
  /// [`PipelineError::BulkheadFull`].
  BulkheadFull,
}

impl PipelineErrorKind {
  /// `failed`, `timeout`, `circuit_open`, `rate_limited` or `bulkhead_full`.
  pub fn label(self) -> &'static str {
    match self {
      Self::Failed => "failed",
      Self::Timeout => "timeout",
      Self::CircuitOpen => "circuit_open",
      Self::RateLimited => "rate_limited",
      Self::BulkheadFull => "bulkhead_full",
    }
  }
}

/// What a [`Pipeline`] reports while it runs a call.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PipelineEvent {
  /// Attempt `attempt` (1-based) is starting.
  Attempt {
    /// 1-based attempt number.
    attempt: u32,
  },
  /// Attempt `attempt` succeeded.
  Success {
    /// 1-based attempt number.
    attempt: u32,
  },
  /// Attempt `attempt` failed or was rejected.
  Failure {
    /// 1-based attempt number.
    attempt: u32,
    /// Why.
    error: PipelineErrorKind,
  },
  /// The fallback answered for a call that ended with `error`.
  Fallback {
    /// The error the fallback handled.
    error: PipelineErrorKind,
  },
}

impl PipelineEvent {
  /// The metric label: `attempt`, `success`, `failure.<kind>` or `fallback.<kind>`.
  pub fn label(&self) -> String {
    match self {
      Self::Attempt { .. } => "attempt".to_string(),
      Self::Success { .. } => "success".to_string(),
      Self::Failure { error, .. } => format!("failure.{}", error.label()),
      Self::Fallback { error } => format!("fallback.{}", error.label()),
    }
  }
}

// ── the pipeline ────────────────────────────────────────────────────────────────────────────

type Listener = Arc<dyn Fn(&str, &PipelineEvent) + Send + Sync>;

/// A reusable composition of resilience policies for calls of type `Effect<A, E, R>` (see the
/// [module docs](self)). Cloning is cheap; clones share the stateful policies (breaker, bulkhead,
/// rate limiter).
pub struct Pipeline<A, E, R = HNil> {
  name: Arc<str>,
  timeout: Option<Duration>,
  retry: Option<Schedule<(), PipelineError<E>>>,
  breaker: Option<CircuitBreaker>,
  bulkhead: Option<Bulkhead>,
  rate_limiter: Option<RateLimiter>,
  fallback: Option<Fallback<A, PipelineError<E>, R>>,
  listeners: Vec<Listener>,
  metric: Option<Frequency>,
}

impl<A, E, R> Clone for Pipeline<A, E, R> {
  fn clone(&self) -> Self {
    Self {
      name: Arc::clone(&self.name),
      timeout: self.timeout,
      retry: self.retry.clone(),
      breaker: self.breaker.clone(),
      bulkhead: self.bulkhead.clone(),
      rate_limiter: self.rate_limiter.clone(),
      fallback: self.fallback.clone(),
      listeners: self.listeners.clone(),
      metric: self.metric.clone(),
    }
  }
}

impl<A, E, R> fmt::Debug for Pipeline<A, E, R> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("Pipeline")
      .field("name", &&*self.name)
      .field("timeout", &self.timeout)
      .field("retry", &self.retry.is_some())
      .field("circuit_breaker", &self.breaker.is_some())
      .field("bulkhead", &self.bulkhead.is_some())
      .field("rate_limiter", &self.rate_limiter.is_some())
      .field("fallback", &self.fallback.is_some())
      .field("listeners", &self.listeners.len())
      .field("metrics", &self.metric.is_some())
      .finish()
  }
}

impl<A, E, R> Pipeline<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  /// An empty pipeline named `name` (the name tags its telemetry). With no policy,
  /// [`execute`](Self::execute) runs the call once.
  pub fn new(name: impl Into<Arc<str>>) -> Self {
    Self {
      name: name.into(),
      timeout: None,
      retry: None,
      breaker: None,
      bulkhead: None,
      rate_limiter: None,
      fallback: None,
      listeners: Vec::new(),
      metric: None,
    }
  }

  /// Fails each attempt that runs longer than `d` with [`PipelineError::Timeout`] (the attempt
  /// is interrupted).
  pub fn timeout(mut self, d: Duration) -> Self {
    self.timeout = Some(d);
    self
  }

  /// Retries per `schedule`, stepped with each [transient](PipelineError::is_transient)
  /// error; any other error ends the retry at once.
  pub fn retry<Out: Send + 'static>(mut self, schedule: Schedule<Out, PipelineError<E>>) -> Self {
    self.retry = Some(
      schedule
        .while_input(PipelineError::is_transient)
        .map(|_| ()),
    );
    self
  }

  /// Guards every attempt with `breaker` (shared with its other handles).
  pub fn circuit_breaker(mut self, breaker: &CircuitBreaker) -> Self {
    self.breaker = Some(breaker.clone());
    self
  }

  /// Runs every attempt under a permit of `bulkhead` (shared with its other handles).
  pub fn bulkhead(mut self, bulkhead: &Bulkhead) -> Self {
    self.bulkhead = Some(bulkhead.clone());
    self
  }

  /// Takes a token of `limiter` before every attempt (shared with its other handles).
  pub fn rate_limiter(mut self, limiter: &RateLimiter) -> Self {
    self.rate_limiter = Some(limiter.clone());
    self
  }

  /// Answers with `fallback` when the call ends with an error it handles.
  pub fn fallback(mut self, fallback: Fallback<A, PipelineError<E>, R>) -> Self {
    self.fallback = Some(fallback);
    self
  }

  /// Adds a listener called synchronously with the pipeline's name and each event.
  pub fn on_event(
    mut self,
    listener: impl Fn(&str, &PipelineEvent) + Send + Sync + 'static,
  ) -> Self {
    self.listeners.push(Arc::new(listener));
    self
  }

  /// Counts every event's [label](PipelineEvent::label) in `metric`, tagged
  /// `pipeline=<name>`.
  pub fn metrics(mut self, metric: &Frequency) -> Self {
    self.metric = Some(metric.tagged("pipeline", self.name.to_string()));
    self
  }

  /// The pipeline's name.
  pub fn name(&self) -> &str {
    &self.name
  }

  /// Runs `make()` through the pipeline. `make` is called once per attempt, when the attempt
  /// starts. Defects and interrupts pass through every policy unchanged.
  pub fn execute(
    &self,
    make: impl Fn() -> Effect<A, E, R> + Send + Sync + 'static,
  ) -> Effect<A, PipelineError<E>, R> {
    let this = self.clone();
    let make = Arc::new(make);
    let attempts = Arc::new(AtomicU32::new(0));
    let attempt = {
      let this = this.clone();
      move || this.attempt(Arc::clone(&make), Arc::clone(&attempts))
    };
    let run = match &self.retry {
      Some(schedule) => Effect::retry(attempt, schedule.clone()),
      None => attempt(),
    };
    let Some(fallback) = self.fallback.clone() else {
      return run;
    };
    run.catch(move |e| {
      if fallback.handles(&e) {
        let error = e.kind();
        this
          .emit(PipelineEvent::Fallback { error })
          .then(fallback.apply(fail(e)))
      } else {
        fail(e)
      }
    })
  }

  /// One attempt: rate limiter, bulkhead, breaker and timeout around `make()`, with telemetry.
  fn attempt<F>(&self, make: Arc<F>, attempts: Arc<AtomicU32>) -> Effect<A, PipelineError<E>, R>
  where
    F: Fn() -> Effect<A, E, R> + Send + Sync + 'static,
  {
    let this = self.clone();
    suspend(move || {
      let n = attempts.fetch_add(1, Ordering::SeqCst) + 1;
      let mut eff = suspend(move || make()).map_err(PipelineError::Failed);
      if let Some(d) = this.timeout {
        eff = eff.timeout_fail(d, || PipelineError::Timeout);
      }
      if let Some(breaker) = &this.breaker {
        eff = breaker.call(eff).map_err(|e| match e {
          Or::Left(_) => PipelineError::CircuitOpen,
          Or::Right(e) => e,
        });
      }
      if let Some(bulkhead) = &this.bulkhead {
        eff = bulkhead.with_permit(eff).map_err(|e| match e {
          Or::Left(_) => PipelineError::BulkheadFull,
          Or::Right(e) => e,
        });
      }
      if let Some(limiter) = &this.rate_limiter {
        eff = limiter
          .acquire()
          .map_err(|_| PipelineError::RateLimited)
          .then(eff);
      }
      let (ok, err) = (this.clone(), this.clone());
      this
        .emit(PipelineEvent::Attempt { attempt: n })
        .then(eff)
        .flat_map(move |a| {
          ok.emit(PipelineEvent::Success { attempt: n })
            .map(move |()| a)
        })
        .catch(move |e| {
          let error = e.kind();
          err
            .emit(PipelineEvent::Failure { attempt: n, error })
            .then(fail(e))
        })
    })
  }

  /// Reports `event` to the listeners and the metric.
  fn emit<E2: Send + 'static>(&self, event: PipelineEvent) -> Effect<(), E2, R> {
    let (listeners, name) = (self.listeners.clone(), Arc::clone(&self.name));
    let notify = sync(move || {
      for listener in &listeners {
        listener(&name, &event);
      }
    });
    match &self.metric {
      Some(metric) => notify.then(metric.occur(event.label())),
      None => notify.then(succeed(())),
    }
  }
}

#[cfg(test)]
mod tests {
  use std::sync::Mutex;

  use id_effect::{sleep, succeed};

  use super::*;
  use crate::test_support::{run_ok, run_virtual};

  type Log = Arc<Mutex<Vec<String>>>;

  fn logged(p: Pipeline<u32, &'static str>) -> (Pipeline<u32, &'static str>, Log) {
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let l = Arc::clone(&log);
    let p = p.on_event(move |name, e| {
      l.lock()
        .expect("lock")
        .push(format!("{name}:{}", e.label()))
    });
    (p, log)
  }

  /// Fails the first `failures` calls, then succeeds with the call number.
  fn flaky(failures: u32) -> impl Fn() -> Effect<u32, &'static str> + Send + Sync + 'static {
    let calls = Arc::new(AtomicU32::new(0));
    move || {
      let calls = Arc::clone(&calls);
      suspend(move || {
        let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
        if n > failures {
          succeed(n)
        } else {
          fail("flaky")
        }
      })
    }
  }

  #[test]
  fn empty_pipeline_runs_once() {
    let (p, log) = logged(Pipeline::new("p"));
    assert_eq!(run_ok(p.execute(flaky(0))), Ok(1));
    assert_eq!(
      run_ok(p.execute(flaky(1))),
      Err(PipelineError::Failed("flaky"))
    );
    assert_eq!(
      *log.lock().expect("lock"),
      ["p:attempt", "p:success", "p:attempt", "p:failure.failed"]
    );
  }

  #[test]
  fn retry_stops_at_non_transient_errors() {
    let breaker = CircuitBreaker::new(2, Duration::from_secs(60));
    let (p, log) = logged(
      Pipeline::new("p")
        .retry(Schedule::recurs(5))
        .circuit_breaker(&breaker),
    );
    // Two failures open the breaker; the third attempt is rejected and not retried.
    assert_eq!(
      run_ok(p.execute(flaky(10))),
      Err(PipelineError::CircuitOpen)
    );
    assert_eq!(
      log.lock().expect("lock").last().map(String::as_str),
      Some("p:failure.circuit_open")
    );
    assert_eq!(log.lock().expect("lock").len(), 6);
  }

  #[test]
  fn timeouts_are_retried_on_virtual_time() {
    let calls = Arc::new(AtomicU32::new(0));
    let c = Arc::clone(&calls);
    let slow_then_fast = move || -> Effect<u32, &'static str> {
      let n = c.fetch_add(1, Ordering::SeqCst);
      let d = if n == 0 { 10 } else { 0 };
      sleep(Duration::from_secs(d)).map(move |()| n)
    };
    let p: Pipeline<u32, &str> = Pipeline::new("p")
      .timeout(Duration::from_secs(1))
      .retry(Schedule::spaced(Duration::from_millis(500)));
    let (exit, elapsed) = run_virtual(p.execute(slow_then_fast));
    assert_eq!(exit.into_result().ok(), Some(1));
    assert_eq!(elapsed, Duration::from_millis(1500));
  }

  #[test]
  fn bulkhead_and_rate_limiter_reject() {
    let p: Pipeline<u32, &str> = Pipeline::new("p").rate_limiter(&RateLimiter::new(0.0, 1.0));
    assert_eq!(run_ok(p.execute(flaky(0))), Ok(1));
    assert_eq!(run_ok(p.execute(flaky(0))), Err(PipelineError::RateLimited));
    let p: Pipeline<u32, &str> = Pipeline::new("p").bulkhead(&Bulkhead::new(0));
    assert_eq!(
      run_ok(p.execute(flaky(0))),
      Err(PipelineError::BulkheadFull)
    );
  }

  #[test]
  fn fallback_handles_selected_errors_and_is_reported() {
    let (p, log) = logged(
      Pipeline::new("p")
        .fallback(Fallback::value(0).when(|e: &PipelineError<&str>| e.is_transient())),
    );
    assert_eq!(run_ok(p.execute(flaky(1))), Ok(0));
    assert_eq!(
      *log.lock().expect("lock"),
      ["p:attempt", "p:failure.failed", "p:fallback.failed"]
    );
    let p = p.bulkhead(&Bulkhead::new(0));
    assert_eq!(
      run_ok(p.execute(flaky(0))),
      Err(PipelineError::BulkheadFull)
    );
  }

  #[test]
  fn metrics_count_events_per_pipeline() {
    let metric = Frequency::new("resilience_pipeline_unit_test_events");
    let p: Pipeline<u32, &str> = Pipeline::new("orders")
      .retry(Schedule::recurs(1))
      .metrics(&metric);
    assert_eq!(run_ok(p.execute(flaky(1))), Ok(2));
    let tagged = metric.tagged("pipeline", "orders");
    assert_eq!(
      tagged.occurrences(),
      [
        ("attempt".to_string(), 2),
        ("failure.failed".to_string(), 1),
        ("success".to_string(), 1)
      ]
    );
  }

  #[test]
  fn errors_display_and_debug() {
    let e: PipelineError<&str> = PipelineError::Failed("boom");
    assert_eq!(e.to_string(), "boom");
    assert_eq!(e.clone().into_failed(), Some("boom"));
    assert_eq!(PipelineError::<&str>::Timeout.to_string(), "timeout");
    assert_eq!(PipelineError::<&str>::RateLimited.into_failed(), None);
    let p: Pipeline<u32, &str> = Pipeline::new("p").timeout(Duration::from_secs(1));
    assert_eq!(p.name(), "p");
    assert!(format!("{p:?}").contains("\"p\""));
  }
}
