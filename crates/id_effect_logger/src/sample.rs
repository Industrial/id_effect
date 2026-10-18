//! Overload protection: per-level sampling and a token-bucket rate limit around a backend.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::{EffectLoggerError, LogBackend, LogLevel, LogRecord};

/// Emitted and dropped line counts of a [`SamplingBackend`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct SamplingStats {
  /// Lines handed to the inner backend.
  pub emitted: u64,
  /// Lines dropped by a per-level ratio ([`SamplingBackend::ratio`]).
  pub sampled_out: u64,
  /// Lines dropped because the token bucket was empty ([`SamplingBackend::rate_limit`]).
  pub rate_limited: u64,
}

impl SamplingStats {
  /// `sampled_out + rate_limited`.
  pub fn dropped(&self) -> u64 {
    self.sampled_out + self.rate_limited
  }
}

#[derive(Debug)]
struct TokenBucket {
  capacity: f64,
  per_second: f64,
  tokens: f64,
  last: Instant,
}

impl TokenBucket {
  fn try_take(&mut self, now: Instant) -> bool {
    let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
    self.last = now;
    self.tokens = (self.tokens + elapsed * self.per_second).min(self.capacity);
    if self.tokens >= 1.0 {
      self.tokens -= 1.0;
      true
    } else {
      false
    }
  }
}

/// Number of emittable levels (`Trace` ..= `Fatal`).
const LEVELS: usize = 6;

fn level_index(level: LogLevel) -> Option<usize> {
  match level {
    LogLevel::None => None,
    l => Some(l.severity() as usize),
  }
}

/// A [`LogBackend`] wrapper that drops lines under load and counts what it dropped.
///
/// - [`Self::ratio`] keeps a deterministic fraction of lines at one level (`0.1` keeps every
///   tenth `Debug` line).
/// - [`Self::rate_limit`] is a token bucket shared by all levels: `burst` lines at once, refilled
///   at `per_second`.
/// - [`Self::exempt_at_or_above`] lets severe lines (e.g. `Error`) bypass both.
///
/// Counts are read with [`Self::stats`].
///
/// ```
/// use std::sync::Arc;
///
/// use id_effect_logger::{LogBackend, LogLevel, LogRecord, SamplingBackend, TracingLogBackend};
///
/// let sampled = SamplingBackend::new(Arc::new(TracingLogBackend))
///   .ratio(LogLevel::Debug, 0.5)
///   .rate_limit(100, 50.0)
///   .exempt_at_or_above(LogLevel::Error);
/// for _ in 0..4 {
///   sampled.emit(&LogRecord::new(LogLevel::Debug, "noisy")).unwrap();
/// }
/// assert_eq!(sampled.stats().emitted, 2);
/// assert_eq!(sampled.stats().sampled_out, 2);
/// ```
pub struct SamplingBackend {
  inner: Arc<dyn LogBackend>,
  ratios: [f64; LEVELS],
  seen: [AtomicU64; LEVELS],
  exempt_from: Option<LogLevel>,
  bucket: Option<Mutex<TokenBucket>>,
  emitted: AtomicU64,
  sampled_out: AtomicU64,
  rate_limited: AtomicU64,
}

impl SamplingBackend {
  /// Wrap `inner`; until configured every line passes.
  pub fn new(inner: Arc<dyn LogBackend>) -> Self {
    Self {
      inner,
      ratios: [1.0; LEVELS],
      seen: Default::default(),
      exempt_from: None,
      bucket: None,
      emitted: AtomicU64::new(0),
      sampled_out: AtomicU64::new(0),
      rate_limited: AtomicU64::new(0),
    }
  }

  /// Keep the fraction `ratio` (clamped to `0.0..=1.0`) of lines at `level`.
  pub fn ratio(mut self, level: LogLevel, ratio: f64) -> Self {
    if let Some(i) = level_index(level) {
      self.ratios[i] = if ratio.is_nan() {
        1.0
      } else {
        ratio.clamp(0.0, 1.0)
      };
    }
    self
  }

  /// Allow at most `burst` lines at once, refilled at `per_second` lines per second (`0.0` makes
  /// `burst` a fixed budget). Applied after [`Self::ratio`], so sampled-out lines cost nothing.
  pub fn rate_limit(mut self, burst: u32, per_second: f64) -> Self {
    let capacity = f64::from(burst);
    self.bucket = Some(Mutex::new(TokenBucket {
      capacity,
      per_second: per_second.max(0.0),
      tokens: capacity,
      last: Instant::now(),
    }));
    self
  }

  /// Lines at `level` or more severe always pass (and are counted as emitted).
  pub fn exempt_at_or_above(mut self, level: LogLevel) -> Self {
    self.exempt_from = Some(level);
    self
  }

  /// Emitted and dropped counts so far.
  pub fn stats(&self) -> SamplingStats {
    SamplingStats {
      emitted: self.emitted.load(Ordering::Relaxed),
      sampled_out: self.sampled_out.load(Ordering::Relaxed),
      rate_limited: self.rate_limited.load(Ordering::Relaxed),
    }
  }

  fn sampled_in(&self, i: usize) -> bool {
    let r = self.ratios[i];
    if r >= 1.0 {
      return true;
    }
    // Deterministic: keep line `n` when the running quota `floor(n * r)` steps up.
    let n = self.seen[i].fetch_add(1, Ordering::Relaxed) as f64;
    ((n + 1.0) * r).floor() > (n * r).floor()
  }

  fn emit_at(&self, rec: &LogRecord<'_>, now: Instant) -> Result<(), EffectLoggerError> {
    let Some(i) = level_index(rec.level) else {
      return self.inner.emit(rec);
    };
    let exempt = self.exempt_from.is_some_and(|min| min.allows(rec.level));
    if !exempt {
      if !self.sampled_in(i) {
        self.sampled_out.fetch_add(1, Ordering::Relaxed);
        return Ok(());
      }
      if let Some(bucket) = &self.bucket {
        let ok = bucket
          .lock()
          .map_err(|e| EffectLoggerError::Sink(format!("sampling bucket mutex: {e}")))?
          .try_take(now);
        if !ok {
          self.rate_limited.fetch_add(1, Ordering::Relaxed);
          return Ok(());
        }
      }
    }
    self.emitted.fetch_add(1, Ordering::Relaxed);
    self.inner.emit(rec)
  }
}

impl LogBackend for SamplingBackend {
  fn emit(&self, rec: &LogRecord<'_>) -> Result<(), EffectLoggerError> {
    self.emit_at(rec, Instant::now())
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::time::Duration;

  struct Count(AtomicU64);
  impl LogBackend for Count {
    fn emit(&self, _rec: &LogRecord<'_>) -> Result<(), EffectLoggerError> {
      self.0.fetch_add(1, Ordering::Relaxed);
      Ok(())
    }
  }

  fn counter() -> Arc<Count> {
    Arc::new(Count(AtomicU64::new(0)))
  }

  #[test]
  fn ratio_keeps_exact_fraction_per_level() {
    let inner = counter();
    let s = SamplingBackend::new(inner.clone())
      .ratio(LogLevel::Debug, 0.25)
      .ratio(LogLevel::Trace, 0.0)
      .ratio(LogLevel::None, 0.0);
    for _ in 0..100 {
      s.emit(&LogRecord::new(LogLevel::Debug, "d")).unwrap();
      s.emit(&LogRecord::new(LogLevel::Trace, "t")).unwrap();
      s.emit(&LogRecord::new(LogLevel::Info, "i")).unwrap();
    }
    let st = s.stats();
    assert_eq!(st.emitted, 125);
    assert_eq!(st.sampled_out, 175);
    assert_eq!(st.dropped(), 175);
    assert_eq!(inner.0.load(Ordering::Relaxed), 125);
  }

  #[test]
  fn nan_ratio_keeps_everything_and_none_level_passes_uncounted() {
    let inner = counter();
    let s = SamplingBackend::new(inner.clone()).ratio(LogLevel::Info, f64::NAN);
    s.emit(&LogRecord::new(LogLevel::Info, "i")).unwrap();
    s.emit(&LogRecord::new(LogLevel::None, "n")).unwrap();
    assert_eq!(s.stats().emitted, 1);
    assert_eq!(inner.0.load(Ordering::Relaxed), 2);
  }

  #[test]
  fn bucket_refills_over_time() {
    let s = SamplingBackend::new(counter()).rate_limit(2, 1.0);
    let t0 = Instant::now();
    let rec = LogRecord::new(LogLevel::Info, "x");
    for _ in 0..3 {
      s.emit_at(&rec, t0).unwrap();
    }
    assert_eq!(s.stats().rate_limited, 1);
    s.emit_at(&rec, t0 + Duration::from_secs(1)).unwrap();
    s.emit_at(&rec, t0 + Duration::from_secs(1)).unwrap();
    let st = s.stats();
    assert_eq!(st.emitted, 3);
    assert_eq!(st.rate_limited, 2);
  }

  #[test]
  fn exempt_levels_bypass_sampling_and_bucket() {
    let s = SamplingBackend::new(counter())
      .ratio(LogLevel::Error, 0.0)
      .rate_limit(0, 0.0)
      .exempt_at_or_above(LogLevel::Error);
    s.emit(&LogRecord::new(LogLevel::Fatal, "f")).unwrap();
    s.emit(&LogRecord::new(LogLevel::Error, "e")).unwrap();
    s.emit(&LogRecord::new(LogLevel::Warn, "w")).unwrap();
    let st = s.stats();
    assert_eq!(st.emitted, 2);
    assert_eq!(st.rate_limited, 1);
  }
}
