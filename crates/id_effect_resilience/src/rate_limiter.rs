//! Token-bucket rate limiter.

use id_effect::coordination::Ref;
use id_effect::kernel::{Effect, box_future};
use id_effect::runtime::{Never, run_async, run_blocking};
use id_effect::scheduling::Clock;

use crate::clock::{NowFn, clock_now, system_now};

/// Returned when no tokens are available.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RateLimitError;

/// Simple token-bucket limiter (`rate` tokens per second, burst `capacity`).
#[derive(Clone)]
pub struct RateLimiter {
  tokens: Ref<f64>,
  last_refill: Ref<std::time::Instant>,
  rate: f64,
  capacity: f64,
  now: NowFn,
}

impl RateLimiter {
  /// Create a limiter with `rate` tokens/sec and maximum burst `capacity`.
  ///
  /// The bucket starts full. [`RateLimiter::acquire`] never waits: it rejects when empty.
  pub fn make(rate: f64, capacity: f64) -> Effect<RateLimiter, Never, ()> {
    Self::make_with_now(rate, capacity, system_now())
  }

  /// Like [`RateLimiter::make`], but refill time is read from `clock`
  /// (e.g. a [`TestClock`](id_effect::scheduling::TestClock) for deterministic tests).
  pub fn make_with_clock<C>(rate: f64, capacity: f64, clock: C) -> Effect<RateLimiter, Never, ()>
  where
    C: Clock + Send + Sync + 'static,
  {
    Self::make_with_now(rate, capacity, clock_now(clock))
  }

  fn make_with_now(rate: f64, capacity: f64, now: NowFn) -> Effect<RateLimiter, Never, ()> {
    Effect::new_async(move |_r: &mut ()| {
      box_future(async move {
        let tokens = run_blocking(Ref::make(capacity), ()).expect("tokens");
        let last_refill = run_blocking(Ref::make(now()), ()).expect("last");
        Ok(RateLimiter {
          tokens,
          last_refill,
          rate,
          capacity,
          now,
        })
      })
    })
  }

  fn refill(&self) -> Effect<(), Never, ()> {
    let me = self.clone();
    Effect::new_async(move |_r: &mut ()| {
      box_future(async move {
        let now = (me.now)();
        let last = run_blocking(me.last_refill.get(), ()).expect("last");
        let elapsed = now.saturating_duration_since(last).as_secs_f64();
        if elapsed > 0.0 {
          let rate = me.rate;
          let capacity = me.capacity;
          run_blocking(
            me.tokens
              .update_and_get(move |t| (t + elapsed * rate).min(capacity)),
            (),
          )
          .expect("refill");
          run_blocking(me.last_refill.set(now), ()).expect("stamp");
        }
        Ok(())
      })
    })
  }

  /// Acquire one token or fail with [`RateLimitError`].
  pub fn acquire(&self) -> Effect<(), RateLimitError, ()> {
    let me = self.clone();
    Effect::new_async(move |_r: &mut ()| {
      box_future(async move {
        run_async(me.refill(), ()).await.expect("refill");
        let ok = run_blocking(
          me.tokens.modify(|t| {
            if t >= 1.0 {
              (true, t - 1.0)
            } else {
              (false, t)
            }
          }),
          (),
        )
        .expect("take");
        if ok { Ok(()) } else { Err(RateLimitError) }
      })
    })
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect::runtime::run_async;
  use id_effect::scheduling::TestClock;
  use std::time::{Duration, Instant};

  #[tokio::test]
  async fn rate_limiter_rejects_when_empty() {
    let rl = run_async(RateLimiter::make(0.0, 1.0), ())
      .await
      .expect("make");
    run_async(rl.acquire(), ()).await.expect("first");
    assert!(matches!(
      run_async(rl.acquire(), ()).await,
      Err(RateLimitError)
    ));
  }

  #[tokio::test]
  async fn rate_limiter_refills_with_elapsed_time() {
    let clock = TestClock::new(Instant::now());
    let rl = run_async(RateLimiter::make_with_clock(2.0, 1.0, clock.clone()), ())
      .await
      .expect("make");
    run_async(rl.acquire(), ()).await.expect("first");
    assert_eq!(run_async(rl.acquire(), ()).await, Err(RateLimitError));
    clock.advance(Duration::from_millis(250));
    assert_eq!(run_async(rl.acquire(), ()).await, Err(RateLimitError));
    clock.advance(Duration::from_millis(250));
    run_async(rl.acquire(), ()).await.expect("refilled");
  }

  #[tokio::test]
  async fn rate_limiter_refill_is_capped_at_capacity() {
    let clock = TestClock::new(Instant::now());
    let rl = run_async(RateLimiter::make_with_clock(1.0, 2.0, clock.clone()), ())
      .await
      .expect("make");
    clock.advance(Duration::from_secs(3600));
    run_async(rl.acquire(), ()).await.expect("burst 1");
    run_async(rl.acquire(), ()).await.expect("burst 2");
    assert_eq!(run_async(rl.acquire(), ()).await, Err(RateLimitError));
  }
}
