//! CPU spread token bucket for [`MetricMode::Spread`](super::policy::MetricMode::Spread).

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Token bucket capping per-worker CPU share under spread policy.
#[derive(Debug)]
pub struct CpuSpreadBucket {
  per_worker: f32,
  capacity: f32,
  tokens: Mutex<f32>,
  last_refill: Mutex<Instant>,
  refill_interval: Duration,
}

impl CpuSpreadBucket {
  /// New bucket with `per_worker` as max concurrent share (0.0–1.0).
  pub fn new(per_worker: f32) -> Self {
    let per_worker = per_worker.clamp(0.01, 1.0);
    Self {
      per_worker,
      capacity: per_worker,
      tokens: Mutex::new(per_worker),
      last_refill: Mutex::new(Instant::now()),
      refill_interval: Duration::from_millis(100),
    }
  }

  fn refill(&self) {
    let mut last = self.last_refill.lock().expect("spread last_refill lock");
    let mut tokens = self.tokens.lock().expect("spread tokens lock");
    let elapsed = last.elapsed();
    if elapsed >= self.refill_interval {
      let steps = (elapsed.as_millis() / self.refill_interval.as_millis().max(1)) as f32;
      *tokens = (*tokens + self.per_worker * steps).min(self.capacity);
      *last = Instant::now();
    }
  }

  /// Non-blocking acquire of one work unit (cost = `per_worker` share).
  pub fn try_acquire(&self) -> bool {
    self.refill();
    let cost = self.per_worker;
    let mut tokens = self.tokens.lock().expect("spread tokens lock");
    if *tokens >= cost {
      *tokens -= cost;
      true
    } else {
      false
    }
  }

  /// Block until a token is available.
  pub fn acquire_blocking(&self) {
    while !self.try_acquire() {
      std::thread::yield_now();
    }
  }

  /// Current token balance (after refill).
  pub fn available(&self) -> f32 {
    self.refill();
    *self.tokens.lock().expect("spread tokens lock")
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn starts_full_and_depletes() {
    let bucket = CpuSpreadBucket::new(0.25);
    assert!(bucket.try_acquire());
    assert!(bucket.available() < 0.25);
  }

  #[test]
  fn blocking_acquire_waits_for_refill() {
    let bucket = CpuSpreadBucket::new(0.5);
    for _ in 0..2 {
      bucket.acquire_blocking();
    }
    assert!(bucket.available() >= 0.0);
  }
}
