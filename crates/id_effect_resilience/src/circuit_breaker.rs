//! Circuit breaker — open after `failure_threshold` failures, probe in half-open after `reset_after`.

use std::time::Duration;

use id_effect::coordination::Ref;
use id_effect::failure::Or;
use id_effect::kernel::{Effect, box_future};
use id_effect::runtime::{Never, run_async, run_blocking};

/// Breaker state machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CircuitState {
  /// Normal operation — effects run.
  Closed,
  /// Fail fast without running the inner effect.
  Open,
  /// Single probe allowed after cooldown.
  HalfOpen,
}

/// Error when the breaker is open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CircuitBreakerError;

/// Tracks failures and gates effects.
#[derive(Clone)]
pub struct CircuitBreaker {
  state: Ref<CircuitState>,
  failures: Ref<u32>,
  failure_threshold: u32,
  reset_after: Duration,
  opened_at: Ref<Option<std::time::Instant>>,
}

impl CircuitBreaker {
  /// Create a breaker in [`CircuitState::Closed`].
  pub fn make(failure_threshold: u32, reset_after: Duration) -> Effect<CircuitBreaker, Never, ()> {
    Effect::new_async(move |_r: &mut ()| {
      box_future(async move {
        let state = run_blocking(Ref::make(CircuitState::Closed), ()).expect("state");
        let failures = run_blocking(Ref::make(0u32), ()).expect("failures");
        let opened_at = run_blocking(Ref::make(None), ()).expect("opened_at");
        Ok(CircuitBreaker {
          state,
          failures,
          failure_threshold,
          reset_after,
          opened_at,
        })
      })
    })
  }

  /// Run `effect` when the breaker allows; record success/failure.
  pub fn call<A, E, R>(&self, effect: Effect<A, E, R>) -> Effect<A, Or<CircuitBreakerError, E>, R>
  where
    A: Send + 'static,
    E: Send + Sync + 'static,
    R: Send + 'static,
  {
    let me = self.clone();
    Effect::new_async(move |r: &mut R| {
      let me = me.clone();
      box_future(async move {
        let state = run_async(me.state.get(), ()).await.expect("state");
        match state {
          CircuitState::Open => {
            let opened = run_async(me.opened_at.get(), ()).await.expect("opened_at");
            if let Some(at) = opened {
              if at.elapsed() >= me.reset_after {
                run_async(me.state.set(CircuitState::HalfOpen), ())
                  .await
                  .expect("half");
              } else {
                return Err(Or::Left(CircuitBreakerError));
              }
            } else {
              return Err(Or::Left(CircuitBreakerError));
            }
          }
          CircuitState::Closed | CircuitState::HalfOpen => {}
        }

        match effect.run(r).await {
          Ok(a) => {
            run_async(me.failures.set(0), ())
              .await
              .expect("reset failures");
            run_async(me.state.set(CircuitState::Closed), ())
              .await
              .expect("close");
            Ok(a)
          }
          Err(e) => {
            let count = run_async(me.failures.update_and_get(|n| n + 1), ())
              .await
              .expect("inc");
            if count >= me.failure_threshold {
              run_async(me.state.set(CircuitState::Open), ())
                .await
                .expect("open");
              run_async(me.opened_at.set(Some(std::time::Instant::now())), ())
                .await
                .expect("opened_at set");
            } else if run_async(me.state.get(), ()).await.expect("state") == CircuitState::HalfOpen
            {
              run_async(me.state.set(CircuitState::Open), ())
                .await
                .expect("reopen");
              run_async(me.opened_at.set(Some(std::time::Instant::now())), ())
                .await
                .expect("opened_at set");
            }
            Err(Or::Right(e))
          }
        }
      })
    })
  }

  /// Current breaker state.
  pub fn state(&self) -> Effect<CircuitState> {
    self.state.get()
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect::failure::Or;
  use id_effect::kernel::fail;
  use id_effect::runtime::run_async;

  #[tokio::test]
  async fn circuit_opens_after_threshold() {
    let cb = run_async(CircuitBreaker::make(2, Duration::from_millis(100)), ())
      .await
      .expect("make");
    assert!(matches!(
      run_async(cb.call(fail::<(), &str, ()>("boom")), ()).await,
      Err(Or::Right(_))
    ));
    assert!(matches!(
      run_async(cb.call(fail::<(), &str, ()>("boom")), ()).await,
      Err(Or::Right(_))
    ));
    assert!(matches!(
      run_async(cb.call(fail::<(), &str, ()>("boom")), ()).await,
      Err(Or::Left(CircuitBreakerError))
    ));
    assert_eq!(
      run_async(cb.state(), ()).await.expect("state"),
      CircuitState::Open
    );
  }
}
