//! A full resilience pipeline (fallback, retry, rate limiter, bulkhead, circuit breaker,
//! timeout) on `TestExecutor` virtual time, with its telemetry.

use std::future::Future;
use std::pin::pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use id_effect::{
  Effect, Executor, Exit, Frequency, Metric, Schedule, TestExecutor, all, fail, sleep, succeed,
  suspend,
};
use id_effect_resilience::{
  Bulkhead, CircuitBreaker, CircuitState, Fallback, Pipeline, PipelineError, PipelineErrorKind,
  PipelineEvent, RateLimiter,
};

/// Runs `eff` to completion on a fresh `TestExecutor`, jumping from timer to timer; returns its
/// exit and the virtual time that passed.
fn run_virtual<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> (Exit<A, E>, Duration) {
  let exec = TestExecutor::new();
  let start = exec.now();
  let h = eff.fork_root(Arc::new(exec.clone()));
  exec.run_until_stalled();
  while !h.is_done() {
    let deadline = exec.next_timer_deadline().expect("stalled with no timer");
    exec.advance(deadline.saturating_duration_since(exec.now()));
  }
  let mut join = pin!(h.join_future());
  let Poll::Ready(exit) = join.as_mut().poll(&mut Context::from_waker(Waker::noop())) else {
    panic!("not done")
  };
  (exit, exec.now().saturating_duration_since(start))
}

type Events = Arc<Mutex<Vec<PipelineEvent>>>;

/// A service whose `n`-th call (1-based) behaves as `script(n)`.
fn service(
  script: impl Fn(u32) -> Effect<u32, &'static str> + Send + Sync + 'static,
) -> (
  impl Fn() -> Effect<u32, &'static str> + Send + Sync + 'static,
  Arc<AtomicU32>,
) {
  let calls = Arc::new(AtomicU32::new(0));
  let c = Arc::clone(&calls);
  let script = Arc::new(script);
  let make = move || {
    let (c, script) = (Arc::clone(&c), Arc::clone(&script));
    suspend(move || script(c.fetch_add(1, Ordering::SeqCst) + 1))
  };
  (make, calls)
}

fn full_pipeline(
  breaker: &CircuitBreaker,
  metric: &Frequency,
) -> (Pipeline<u32, &'static str>, Events) {
  let events: Events = Arc::new(Mutex::new(Vec::new()));
  let log = Arc::clone(&events);
  let pipeline = Pipeline::new("quotes")
    .timeout(Duration::from_secs(1))
    .retry(Schedule::spaced(Duration::from_millis(100)).intersect(Schedule::recurs(3)))
    .rate_limiter(&RateLimiter::new(1.0, 100.0))
    .bulkhead(&Bulkhead::new(2))
    .circuit_breaker(breaker)
    .fallback(
      Fallback::new(|_| succeed(0))
        .when(|e: &PipelineError<&str>| *e == PipelineError::CircuitOpen),
    )
    .on_event(move |name, e| {
      assert_eq!(name, "quotes");
      log.lock().expect("lock").push(*e);
    })
    .metrics(metric);
  (pipeline, events)
}

#[test]
fn timeout_then_failure_then_success_on_virtual_time() {
  let breaker = CircuitBreaker::new(5, Duration::from_secs(30));
  let metric = Frequency::new("resilience_it_events_a");
  let (pipeline, events) = full_pipeline(&breaker, &metric);
  let (make, calls) = service(|n| match n {
    1 => sleep(Duration::from_secs(10)).map(|()| 0),
    2 => fail("503"),
    _ => succeed(7),
  });
  let (exit, elapsed) = run_virtual(pipeline.execute(make));
  assert_eq!(exit, Exit::Success(7));
  // 1 s timeout, 100 ms backoff, an immediate 503, 100 ms backoff, success.
  assert_eq!(elapsed, Duration::from_millis(1200));
  assert_eq!(calls.load(Ordering::SeqCst), 3);
  use PipelineEvent::*;
  assert_eq!(
    *events.lock().expect("lock"),
    [
      Attempt { attempt: 1 },
      Failure {
        attempt: 1,
        error: PipelineErrorKind::Timeout
      },
      Attempt { attempt: 2 },
      Failure {
        attempt: 2,
        error: PipelineErrorKind::Failed
      },
      Attempt { attempt: 3 },
      Success { attempt: 3 },
    ]
  );
  assert_eq!(
    metric.tagged("pipeline", "quotes").occurrences(),
    [
      ("attempt".to_string(), 3),
      ("failure.failed".to_string(), 1),
      ("failure.timeout".to_string(), 1),
      ("success".to_string(), 1),
    ]
  );
}

#[test]
fn outage_opens_the_breaker_falls_back_and_recovers_after_the_cooldown() {
  let breaker = CircuitBreaker::new(3, Duration::from_secs(30));
  let metric = Frequency::new("resilience_it_events_b");
  let (pipeline, events) = full_pipeline(&breaker, &metric);
  let (down, down_calls) = service(|_| fail("503"));
  let (up, up_calls) = service(|_| succeed(9));
  let state = breaker.clone();
  let program = pipeline
    .execute(down)
    .zip(state.state())
    .flat_map(move |(during, s)| {
      sleep(Duration::from_secs(30))
        .then(pipeline.execute(up))
        .map(move |after| (during, s, after))
    });
  let (exit, _) = run_virtual(program);
  let (during, open, after) = exit.into_result().expect("program");
  // Three failures open the breaker; the fourth attempt is rejected and the fallback answers.
  assert_eq!(during, 0);
  assert_eq!(open, CircuitState::Open);
  assert_eq!(down_calls.load(Ordering::SeqCst), 3);
  assert!(
    events
      .lock()
      .expect("lock")
      .contains(&PipelineEvent::Fallback {
        error: PipelineErrorKind::CircuitOpen
      })
  );
  // After the cooldown the half-open probe succeeds.
  assert_eq!(after, 9);
  assert_eq!(up_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn bulkhead_rejections_are_not_retried_and_not_handled_by_the_fallback() {
  let breaker = CircuitBreaker::new(5, Duration::from_secs(30));
  let metric = Frequency::new("resilience_it_events_c");
  let (pipeline, _) = full_pipeline(&breaker, &metric);
  let (slow, _) = service(|_| sleep(Duration::from_millis(500)).map(|()| 1));
  let slow = Arc::new(slow);
  let calls: Vec<Effect<Result<u32, PipelineError<&str>>>> = (0..3)
    .map(|_| {
      let slow = Arc::clone(&slow);
      pipeline
        .execute(move || slow())
        .map(Ok)
        .catch(|e| succeed(Err(e)))
    })
    .collect();
  let (exit, elapsed) = run_virtual(all(calls, id_effect::Concurrency::Unbounded));
  let results = exit.into_result().expect("program");
  assert_eq!(results.iter().filter(|r| **r == Ok(1)).count(), 2);
  assert!(results.contains(&Err(PipelineError::BulkheadFull)));
  assert_eq!(elapsed, Duration::from_millis(500));
}
