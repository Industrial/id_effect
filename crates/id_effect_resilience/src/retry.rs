//! Retry through a [`CircuitBreaker`] according to a core [`Schedule`].
//!
//! Each attempt goes through [`CircuitBreaker::call`], so successes and failures are recorded in
//! the breaker. Failures are retried while the [`Schedule`] allows; once the breaker is open the
//! loop stops immediately with [`CircuitBreakerError`] instead of sleeping through more attempts.

use id_effect::failure::Or;
use id_effect::kernel::{Effect, box_future};
use id_effect::runtime::ThreadSleepRuntime;
use id_effect::scheduling::{Clock, LiveClock, Schedule, ScheduleInput};

use crate::circuit_breaker::{CircuitBreaker, CircuitBreakerError};

/// Retry `make()` per `schedule`, guarded by `breaker`; sleeps with a live runtime clock.
///
/// Outcomes:
/// - `Ok(a)` — an attempt succeeded (the breaker is closed again);
/// - `Err(Or::Right(e))` — the schedule was exhausted; `e` is the last failure;
/// - `Err(Or::Left(CircuitBreakerError))` — the breaker was (or became) open, so retrying stopped.
///
/// See [`retry_with_breaker_and_clock`] to inject a [`Clock`] (e.g. a
/// [`TestClock`](id_effect::scheduling::TestClock)).
pub fn retry_with_breaker<A, E, R, F>(
  make: F,
  schedule: Schedule,
  breaker: &CircuitBreaker,
) -> Effect<A, Or<CircuitBreakerError, E>, R>
where
  A: Send + 'static,
  E: Send + Sync + 'static,
  R: Send + 'static,
  F: FnMut() -> Effect<A, E, R> + 'static,
{
  retry_with_breaker_and_clock(
    make,
    schedule,
    breaker,
    LiveClock::new(ThreadSleepRuntime::default()),
  )
}

/// Like [`retry_with_breaker`], but backoff delays are slept on `clock`.
pub fn retry_with_breaker_and_clock<A, E, R, F, C>(
  mut make: F,
  mut schedule: Schedule,
  breaker: &CircuitBreaker,
  clock: C,
) -> Effect<A, Or<CircuitBreakerError, E>, R>
where
  A: Send + 'static,
  E: Send + Sync + 'static,
  R: Send + 'static,
  F: FnMut() -> Effect<A, E, R> + 'static,
  C: Clock + Clone + 'static,
{
  let breaker = breaker.clone();
  Effect::new_async(move |r: &mut R| {
    box_future(async move {
      let mut attempt = 0_u64;
      loop {
        match breaker.call(make()).run(r).await {
          Ok(a) => return Ok(a),
          Err(Or::Left(open)) => return Err(Or::Left(open)),
          Err(Or::Right(e)) => match schedule.next_sleep(&clock, ScheduleInput { attempt }) {
            Some(sleep) => {
              match sleep.run(&mut ()).await {
                Ok(()) => {}
                Err(never) => match never {},
              }
              attempt = attempt.saturating_add(1);
            }
            None => return Err(Or::Right(e)),
          },
        }
      }
    })
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::circuit_breaker::CircuitState;
  use id_effect::kernel::fail;
  use id_effect::runtime::run_async;
  use id_effect::scheduling::TestClock;
  use std::sync::Arc;
  use std::sync::atomic::{AtomicU32, Ordering};
  use std::time::{Duration, Instant};

  /// Factory that fails until attempt `succeed_on` (1-based), counting every run.
  fn flaky(
    runs: &Arc<AtomicU32>,
    succeed_on: u32,
  ) -> impl FnMut() -> Effect<u32, &'static str, ()> + 'static {
    let runs = Arc::clone(runs);
    move || {
      let runs = Arc::clone(&runs);
      Effect::new(move |_r: &mut ()| {
        let n = runs.fetch_add(1, Ordering::SeqCst) + 1;
        if n >= succeed_on { Ok(n) } else { Err("flaky") }
      })
    }
  }

  async fn breaker(threshold: u32, clock: &TestClock) -> CircuitBreaker {
    run_async(
      CircuitBreaker::make_with_clock(threshold, Duration::from_secs(60), clock.clone()),
      (),
    )
    .await
    .expect("make")
  }

  #[tokio::test]
  async fn retries_until_success_and_leaves_breaker_closed() {
    let clock = TestClock::new(Instant::now());
    let cb = breaker(5, &clock).await;
    let runs = Arc::new(AtomicU32::new(0));
    let out = run_async(
      retry_with_breaker_and_clock(
        flaky(&runs, 3),
        Schedule::spaced(Duration::from_millis(100)),
        &cb,
        clock.clone(),
      ),
      (),
    )
    .await;
    assert_eq!(out.ok(), Some(3));
    assert_eq!(runs.load(Ordering::SeqCst), 3);
    assert_eq!(
      run_async(cb.state(), ()).await.expect("state"),
      CircuitState::Closed
    );
  }

  #[tokio::test]
  async fn stops_retrying_once_breaker_opens() {
    let clock = TestClock::new(Instant::now());
    let cb = breaker(2, &clock).await;
    let runs = Arc::new(AtomicU32::new(0));
    let out = run_async(
      retry_with_breaker_and_clock(flaky(&runs, u32::MAX), Schedule::recurs(10), &cb, clock),
      (),
    )
    .await;
    assert!(matches!(out, Err(Or::Left(CircuitBreakerError))));
    assert_eq!(runs.load(Ordering::SeqCst), 2);
    assert_eq!(
      run_async(cb.state(), ()).await.expect("state"),
      CircuitState::Open
    );
  }

  #[tokio::test]
  async fn exhausted_schedule_returns_last_error() {
    let clock = TestClock::new(Instant::now());
    let cb = breaker(10, &clock).await;
    let runs = Arc::new(AtomicU32::new(0));
    let out = run_async(
      retry_with_breaker_and_clock(flaky(&runs, u32::MAX), Schedule::recurs(2), &cb, clock),
      (),
    )
    .await;
    assert!(matches!(out, Err(Or::Right("flaky"))));
    assert_eq!(runs.load(Ordering::SeqCst), 3);
    assert_eq!(
      run_async(cb.state(), ()).await.expect("state"),
      CircuitState::Closed
    );
  }

  #[tokio::test]
  async fn already_open_breaker_fails_fast_without_running() {
    let clock = TestClock::new(Instant::now());
    let cb = breaker(1, &clock).await;
    let _ = run_async(cb.call(fail::<(), &str, ()>("boom")), ()).await;
    let runs = Arc::new(AtomicU32::new(0));
    let out = run_async(
      retry_with_breaker_and_clock(flaky(&runs, 1), Schedule::recurs(3), &cb, clock),
      (),
    )
    .await;
    assert!(matches!(out, Err(Or::Left(CircuitBreakerError))));
    assert_eq!(runs.load(Ordering::SeqCst), 0);
  }

  #[tokio::test]
  async fn half_open_probe_success_closes_breaker() {
    let clock = TestClock::new(Instant::now());
    let cb = breaker(1, &clock).await;
    let _ = run_async(cb.call(fail::<(), &str, ()>("boom")), ()).await;
    clock.advance(Duration::from_secs(60));
    let runs = Arc::new(AtomicU32::new(0));
    let out = run_async(
      retry_with_breaker_and_clock(flaky(&runs, 1), Schedule::recurs(3), &cb, clock),
      (),
    )
    .await;
    assert_eq!(out.ok(), Some(1));
    assert_eq!(
      run_async(cb.state(), ()).await.expect("state"),
      CircuitState::Closed
    );
  }

  #[tokio::test]
  async fn live_clock_variant_retries_with_zero_delay_schedule() {
    let cb = run_async(CircuitBreaker::make(5, Duration::from_secs(60)), ())
      .await
      .expect("make");
    let runs = Arc::new(AtomicU32::new(0));
    let out = run_async(
      retry_with_breaker(flaky(&runs, 2), Schedule::recurs(3), &cb),
      (),
    )
    .await;
    assert_eq!(out.ok(), Some(2));
    assert_eq!(runs.load(Ordering::SeqCst), 2);
  }

  #[tokio::test]
  async fn success_between_failures_resets_the_breaker_count() {
    let clock = TestClock::new(Instant::now());
    let cb = breaker(2, &clock).await;
    let runs = Arc::new(AtomicU32::new(0));
    // One failure, then success: breaker count resets, so the next single failure must not open it.
    let first = run_async(
      retry_with_breaker_and_clock(flaky(&runs, 2), Schedule::recurs(3), &cb, clock.clone()),
      (),
    )
    .await;
    assert_eq!(first.ok(), Some(2));
    let _ = run_async(cb.call(fail::<(), &str, ()>("once")), ()).await;
    assert_eq!(
      run_async(cb.state(), ()).await.expect("state"),
      CircuitState::Closed
    );
  }

  #[tokio::test]
  async fn backoff_delays_are_slept_on_the_injected_clock() {
    let clock = TestClock::new(Instant::now());
    let start = clock.now();
    let cb = breaker(10, &clock).await;
    let runs = Arc::new(AtomicU32::new(0));
    let out = run_async(
      retry_with_breaker_and_clock(
        flaky(&runs, 3),
        Schedule::exponential(Duration::from_millis(100)),
        &cb,
        clock.clone(),
      ),
      (),
    )
    .await;
    assert_eq!(out.ok(), Some(3));
    assert_eq!(
      clock.pending_sleeps(),
      vec![
        start + Duration::from_millis(100),
        start + Duration::from_millis(200)
      ]
    );
  }
}
