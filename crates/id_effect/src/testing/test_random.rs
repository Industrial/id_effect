//! `TestRandom`: a deterministic [`RandomSource`] with queued values (Effect.ts `TestRandom`).

use std::collections::VecDeque;
use std::fmt;
use std::ops::Range;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::random::{Random, RandomSource, Xoshiro256, uniform_in, unit_f64};

/// A deterministic random source for tests: a seeded xoshiro256** generator plus queues of
/// values to return first (Effect.ts `TestRandom.feedInts` / `feedDoubles`).
///
/// - [`seeded`](Self::seeded): the same seed always gives the same sequence. It is the sequence
///   [`Random::seeded`] gives for that seed.
/// - [`feed_u64s`](Self::feed_u64s) queues values for `next_u64` and `next_range`;
///   [`feed_f64s`](Self::feed_f64s) queues values for `next_f64`. A queued value is returned
///   before the generator is drawn from, in FIFO order; the generator does not advance while a
///   queued value is used. The two queues are independent.
/// - `next_range` maps a queued `v` to `start + (v - start) % (end - start)` (wrapping
///   subtraction), so a fed value inside the range comes back unchanged; an empty range returns
///   `start` and consumes nothing. `shuffle` draws its indices with `next_range`, so fed values
///   drive it too.
///
/// Cloning gives another handle to the same state, so a test keeps one handle to feed values
/// while the effect under test reads another through its [`Random`] capability:
///
/// ```
/// use id_effect::{Effect, HCons, HNil, Random, TestRandom, random_u64};
/// use id_effect::Exit;
///
/// let rng = TestRandom::seeded(42);
/// rng.feed_u64s([7, 8]);
/// let eff: Effect<Vec<u64>, (), HCons<Random, HNil>> =
///   random_u64().flat_map(|a| random_u64().map(move |b| vec![a, b]));
/// let exit = eff.provide(rng.random()).block_on();
/// assert!(matches!(exit, Exit::Success(v) if v == [7, 8]));
/// ```
#[derive(Clone)]
pub struct TestRandom {
  inner: Arc<Mutex<State>>,
}

struct State {
  rng: Xoshiro256,
  u64s: VecDeque<u64>,
  f64s: VecDeque<f64>,
}

impl TestRandom {
  /// A test source seeded with `seed`, with empty queues.
  pub fn seeded(seed: u64) -> Self {
    Self {
      inner: Arc::new(Mutex::new(State {
        rng: Xoshiro256::seeded(seed),
        u64s: VecDeque::new(),
        f64s: VecDeque::new(),
      })),
    }
  }

  fn state(&self) -> MutexGuard<'_, State> {
    self.inner.lock().unwrap_or_else(PoisonError::into_inner)
  }

  /// Queues `values` for `next_u64` and `next_range` (after any already queued).
  pub fn feed_u64s(&self, values: impl IntoIterator<Item = u64>) {
    self.state().u64s.extend(values);
  }

  /// Queues `values` for `next_f64` (after any already queued). A value outside `[0, 1)` is
  /// clamped into it (NaN and negatives to `0.0`, `1.0` and above to the largest `f64` below
  /// `1.0`), so `next_f64` keeps its contract.
  pub fn feed_f64s(&self, values: impl IntoIterator<Item = f64>) {
    const BELOW_ONE: f64 = 1.0 - f64::EPSILON / 2.0;
    self
      .state()
      .f64s
      .extend(values.into_iter().map(|v| match v {
        v if v >= 1.0 => BELOW_ONE,
        v if v >= 0.0 => v,
        _ => 0.0,
      }));
  }

  /// Drops every queued value.
  pub fn clear_feeds(&self) {
    let mut s = self.state();
    s.u64s.clear();
    s.f64s.clear();
  }

  /// A [`Random`] capability over this source (sharing its state).
  pub fn random(&self) -> Random {
    Random::from_source(self.clone())
  }
}

impl RandomSource for TestRandom {
  fn next_u64(&self) -> u64 {
    let mut s = self.state();
    match s.u64s.pop_front() {
      Some(v) => v,
      None => s.rng.next_u64(),
    }
  }

  fn next_f64(&self) -> f64 {
    let mut s = self.state();
    match s.f64s.pop_front() {
      Some(v) => v,
      None => unit_f64(s.rng.next_u64()),
    }
  }

  fn next_range(&self, range: Range<u64>) -> u64 {
    if range.start >= range.end {
      return range.start;
    }
    let mut s = self.state();
    match s.u64s.pop_front() {
      Some(v) => range.start + v.wrapping_sub(range.start) % (range.end - range.start),
      None => uniform_in(range, || s.rng.next_u64()),
    }
  }
}

impl From<TestRandom> for Random {
  fn from(t: TestRandom) -> Self {
    Random::from_source(t)
  }
}

impl fmt::Debug for TestRandom {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let s = self.state();
    f.debug_struct("TestRandom")
      .field("queued_u64s", &s.u64s.len())
      .field("queued_f64s", &s.f64s.len())
      .finish_non_exhaustive()
  }
}
