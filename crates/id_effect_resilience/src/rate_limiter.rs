//! Token-bucket rate limiter.

use id_effect::coordination::Ref;
use id_effect::kernel::{Effect, box_future};
use id_effect::runtime::{Never, run_async, run_blocking};

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
}

impl RateLimiter {
  /// Create a limiter with `rate` tokens/sec and maximum burst `capacity`.
  pub fn make(rate: f64, capacity: f64) -> Effect<RateLimiter, Never, ()> {
    Effect::new_async(move |_r: &mut ()| {
      box_future(async move {
        let tokens = run_blocking(Ref::make(capacity), ()).expect("tokens");
        let last_refill = run_blocking(Ref::make(std::time::Instant::now()), ()).expect("last");
        Ok(RateLimiter {
          tokens,
          last_refill,
          rate,
          capacity,
        })
      })
    })
  }

  fn refill(&self) -> Effect<(), Never, ()> {
    let me = self.clone();
    Effect::new_async(move |_r: &mut ()| {
      box_future(async move {
        let now = std::time::Instant::now();
        let last = run_blocking(me.last_refill.get(), ()).expect("last");
        let elapsed = now.duration_since(last).as_secs_f64();
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
}
