//! Token-bucket rate limiter.
//!
//! Refill is measured on the running fiber's executor clock, so on a
//! [`TestExecutor`](id_effect::TestExecutor) it follows virtual time.

use std::time::Instant;

use id_effect::{Effect, Ref, fail, succeed};

use crate::clock::now;

/// Returned when no tokens are available.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RateLimitError;

/// The bucket's mutable state. `last` is `None` until the first acquire stamps it.
#[derive(Clone, Copy, Debug)]
struct Bucket {
  tokens: f64,
  last: Option<Instant>,
}

/// Simple token-bucket limiter (`rate` tokens per second, burst `capacity`). Cloning yields
/// another handle to the same bucket.
#[derive(Clone, Debug)]
pub struct RateLimiter {
  bucket: Ref<Bucket>,
  rate: f64,
  capacity: f64,
}

impl RateLimiter {
  /// A limiter with `rate` tokens/sec and maximum burst `capacity`.
  ///
  /// The bucket starts full, and refill time is counted from the first
  /// [`acquire`](Self::acquire). `acquire` never waits: it rejects when the bucket is empty.
  pub fn new(rate: f64, capacity: f64) -> Self {
    Self {
      bucket: Ref::new(Bucket {
        tokens: capacity,
        last: None,
      }),
      rate,
      capacity,
    }
  }

  /// Takes one token, or fails with [`RateLimitError`] when none is available.
  pub fn acquire<R>(&self) -> Effect<(), RateLimitError, R>
  where
    R: Send + Sync + 'static,
  {
    let (bucket, rate, capacity) = (self.bucket.clone(), self.rate, self.capacity);
    now()
      .flat_map(move |t| {
        bucket.modify(move |b: Bucket| {
          let tokens = match b.last {
            Some(last) => {
              (b.tokens + t.saturating_duration_since(last).as_secs_f64() * rate).min(capacity)
            }
            None => b.tokens,
          };
          let last = Some(t);
          if tokens >= 1.0 {
            (
              true,
              Bucket {
                tokens: tokens - 1.0,
                last,
              },
            )
          } else {
            (false, Bucket { tokens, last })
          }
        })
      })
      .flat_map(|ok| {
        if ok {
          succeed(())
        } else {
          fail(RateLimitError)
        }
      })
  }
}

#[cfg(test)]
mod tests {
  use std::time::Duration;

  use id_effect::sleep;

  use super::*;
  use crate::test_support::{run_ok, run_virtual};

  type Taken = Result<(), RateLimitError>;

  fn take(rl: &RateLimiter) -> Effect<Taken, RateLimitError> {
    rl.acquire().map(Ok).catch(|e| succeed(Err(e)))
  }

  #[test]
  fn rate_limiter_rejects_when_empty() {
    let rl = RateLimiter::new(0.0, 1.0);
    let out = run_ok(take(&rl).zip(take(&rl)));
    assert_eq!(out, Ok((Ok(()), Err(RateLimitError))));
  }

  #[test]
  fn rate_limiter_refills_with_virtual_time() {
    let rl = RateLimiter::new(2.0, 1.0);
    let quarter = || sleep(Duration::from_millis(250));
    let prog = take(&rl)
      .zip(take(&rl))
      .zip(quarter().then(take(&rl)))
      .zip(quarter().then(take(&rl)));
    let (exit, elapsed) = run_virtual(prog);
    let (((first, empty), still_empty), refilled) = exit.into_result().expect("program");
    assert_eq!(first, Ok(()));
    assert_eq!(empty, Err(RateLimitError));
    assert_eq!(
      still_empty,
      Err(RateLimitError),
      "half a token after 250 ms"
    );
    assert_eq!(refilled, Ok(()), "a whole token after 500 ms");
    assert_eq!(elapsed, Duration::from_millis(500));
  }

  #[test]
  fn rate_limiter_refill_is_capped_at_capacity() {
    let rl = RateLimiter::new(1.0, 2.0);
    let prog = take(&rl)
      .then(sleep(Duration::from_secs(3600)))
      .then(take(&rl))
      .zip(take(&rl))
      .zip(take(&rl));
    let ((a, b), c) = run_ok(prog).expect("program");
    assert_eq!((a, b, c), (Ok(()), Ok(()), Err(RateLimitError)));
  }

  #[test]
  fn clones_share_one_bucket() {
    let rl = RateLimiter::new(0.0, 1.0);
    let other = rl.clone();
    let out = run_ok(take(&rl).zip(take(&other)));
    assert_eq!(out, Ok((Ok(()), Err(RateLimitError))));
  }
}
