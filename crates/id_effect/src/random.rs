//! The `Random` capability: `next_u64`, `next_f64` in `[0, 1)`, `next_range` and `shuffle`, on
//! an internal xoshiro256** generator (no new dependency, Q13). The deterministic `TestRandom`
//! (`seeded`, `feed_u64s`/`feed_f64s`) is exported from [`testing`](crate::next::testing).
//!
//! # As a capability
//!
//! [`Random`] is an ordinary capability: an effect that draws random numbers names it in its
//! environment `R` and reads it with [`need`](crate::next::need) (or the accessors
//! [`random_u64`], [`random_f64`], [`random_range`] and [`random_shuffle`]). The edge provides
//! [`Random::live`] (seeded from OS entropy) in production and a seeded
//! [`TestRandom`](crate::next::TestRandom) in tests.
//!
//! ```
//! use id_effect::next::{Effect, HCons, HNil, Random, random_range};
//! use id_effect::Exit;
//!
//! fn roll() -> Effect<u64, (), HCons<Random, HNil>> {
//!   random_range(1..7)
//! }
//!
//! let Exit::Success(n) = roll().provide(Random::seeded(42)).block_on() else {
//!   unreachable!()
//! };
//! assert!((1..7).contains(&n));
//! ```
//!
//! # Outside an effect: [`RandomSource`]
//!
//! Code that is not itself an effect (a `Schedule` step computing a jittered delay, say) takes a
//! `&dyn RandomSource` (or a [`Random`], which is one and is cheap to clone) and calls
//! [`RandomSource::next_f64`] / [`RandomSource::next_range`] directly. [`Random`] and
//! `TestRandom` both implement it, so a schedule driven under a `TestRandom` is deterministic:
//!
//! ```
//! use std::time::Duration;
//! use id_effect::next::{Random, RandomSource};
//!
//! /// `base` scaled by a factor in `[0.8, 1.2)`.
//! fn jitter(base: Duration, rng: &dyn RandomSource) -> Duration {
//!   base.mul_f64(0.8 + 0.4 * rng.next_f64())
//! }
//!
//! let d = jitter(Duration::from_millis(100), &Random::seeded(7));
//! assert!(d >= Duration::from_millis(80) && d < Duration::from_millis(120));
//! ```
//!
//! # Algorithms
//!
//! - Generator: xoshiro256** (Blackman and Vigna), its 256-bit state filled from a 64-bit seed
//!   with SplitMix64, as the reference implementation recommends. Not cryptographically secure.
//! - `next_f64`: the top 53 bits of `next_u64`, times 2^-53, so the result is in `[0, 1)`.
//! - `next_range`: Lemire's multiply-and-reject method, so the result is unbiased.
//! - `shuffle`: Fisher–Yates, from the last element down, drawing each swap index with
//!   `next_range`.

use std::collections::hash_map::RandomState;
use std::fmt;
use std::hash::{BuildHasher, Hash, Hasher};
use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::next::capability::{Selector, need};
use crate::next::kernel::Effect;

// ── generator ───────────────────────────────────────────────────────────────────────────────

/// One SplitMix64 step: advances `state` and returns the next output.
pub(crate) fn splitmix64(state: &mut u64) -> u64 {
  *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
  let mut z = *state;
  z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
  z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
  z ^ (z >> 31)
}

/// The xoshiro256** generator (state only; not synchronised).
#[derive(Debug, Clone)]
pub(crate) struct Xoshiro256 {
  s: [u64; 4],
}

impl Xoshiro256 {
  /// Fills the state from `seed` with SplitMix64. The state is never all zero (four consecutive
  /// SplitMix64 outputs cannot all be zero).
  pub(crate) fn seeded(seed: u64) -> Self {
    let mut sm = seed;
    Self {
      s: [
        splitmix64(&mut sm),
        splitmix64(&mut sm),
        splitmix64(&mut sm),
        splitmix64(&mut sm),
      ],
    }
  }

  /// A generator with exactly this state (for reference-vector tests).
  #[cfg(test)]
  fn from_state(s: [u64; 4]) -> Self {
    Self { s }
  }

  pub(crate) fn next_u64(&mut self) -> u64 {
    let s = &mut self.s;
    let result = s[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
    let t = s[1] << 17;
    s[2] ^= s[0];
    s[3] ^= s[1];
    s[1] ^= s[2];
    s[0] ^= s[3];
    s[2] ^= t;
    s[3] = s[3].rotate_left(45);
    result
  }
}

/// Maps a `u64` to `[0, 1)` (its top 53 bits times 2^-53).
pub(crate) fn unit_f64(x: u64) -> f64 {
  const SCALE: f64 = 1.0 / (1u64 << 53) as f64;
  (x >> 11) as f64 * SCALE
}

/// A uniform value in `range` drawn with Lemire's method from `next`; `range.start` when the
/// range is empty.
pub(crate) fn uniform_in(range: Range<u64>, mut next: impl FnMut() -> u64) -> u64 {
  if range.start >= range.end {
    return range.start;
  }
  let span = range.end - range.start;
  let mut m = u128::from(next()) * u128::from(span);
  if (m as u64) < span {
    let threshold = span.wrapping_neg() % span;
    while (m as u64) < threshold {
      m = u128::from(next()) * u128::from(span);
    }
  }
  range.start + (m >> 64) as u64
}

/// A 64-bit seed from OS-seeded hashing keys, the wall clock, the thread and a process counter.
fn entropy_seed() -> u64 {
  static CALLS: AtomicU64 = AtomicU64::new(0);
  let mut h = RandomState::new().build_hasher();
  CALLS.fetch_add(1, Ordering::Relaxed).hash(&mut h);
  std::thread::current().id().hash(&mut h);
  if let Ok(t) = SystemTime::now().duration_since(UNIX_EPOCH) {
    t.as_nanos().hash(&mut h);
  }
  h.finish()
}

// ── RandomSource ────────────────────────────────────────────────────────────────────────────

/// A source of random numbers that is not an effect: the non-effect API that schedules and
/// other pure code call (see the [module docs](self)).
///
/// Only [`next_u64`](Self::next_u64) is required; the default `next_f64` and `next_range` derive
/// from it. A source that queues test values (`TestRandom`) overrides them so that queued values
/// come back verbatim.
pub trait RandomSource: Send + Sync {
  /// A uniformly distributed `u64`.
  fn next_u64(&self) -> u64;

  /// A uniformly distributed `f64` in `[0, 1)`.
  fn next_f64(&self) -> f64 {
    unit_f64(self.next_u64())
  }

  /// A uniformly distributed value in `range` (half-open). An empty range returns
  /// `range.start` and draws nothing.
  fn next_range(&self, range: Range<u64>) -> u64 {
    uniform_in(range, || self.next_u64())
  }
}

/// Shuffles `items` in place (Fisher–Yates) with indices drawn from `source`.
pub fn shuffle_with<T>(source: &(impl RandomSource + ?Sized), items: &mut [T]) {
  for i in (1..items.len()).rev() {
    let j = source.next_range(0..i as u64 + 1) as usize;
    items.swap(i, j);
  }
}

/// A xoshiro256** generator behind a mutex.
struct Locked(Mutex<Xoshiro256>);

impl RandomSource for Locked {
  fn next_u64(&self) -> u64 {
    self
      .0
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .next_u64()
  }
}

// ── Random ──────────────────────────────────────────────────────────────────────────────────

/// The random-number capability: a shared handle to a [`RandomSource`].
///
/// Cloning gives another handle to the same source (clones draw from one sequence). Use it as an
/// environment capability (`need::<Random>()`, [`random_u64`], …) or call its methods directly.
#[derive(Clone)]
pub struct Random {
  source: Arc<dyn RandomSource>,
}

impl Random {
  /// A generator seeded from OS entropy (the hashing keys of
  /// [`RandomState`], mixed with the clock, the thread and a counter). Every call gives an
  /// independent sequence.
  pub fn live() -> Self {
    Self::seeded(entropy_seed())
  }

  /// A deterministic generator: the same seed always gives the same sequence.
  pub fn seeded(seed: u64) -> Self {
    Self::from_source(Locked(Mutex::new(Xoshiro256::seeded(seed))))
  }

  /// Wraps any [`RandomSource`] (for example a `TestRandom`).
  pub fn from_source(source: impl RandomSource + 'static) -> Self {
    Self {
      source: Arc::new(source),
    }
  }

  /// A uniformly distributed `u64`.
  pub fn next_u64(&self) -> u64 {
    self.source.next_u64()
  }

  /// A uniformly distributed `f64` in `[0, 1)`.
  pub fn next_f64(&self) -> f64 {
    self.source.next_f64()
  }

  /// A uniformly distributed value in `range`; `range.start` when it is empty.
  pub fn next_range(&self, range: Range<u64>) -> u64 {
    self.source.next_range(range)
  }

  /// Shuffles `items` in place.
  pub fn shuffle<T>(&self, items: &mut [T]) {
    shuffle_with(&*self.source, items);
  }
}

impl RandomSource for Random {
  fn next_u64(&self) -> u64 {
    self.source.next_u64()
  }

  fn next_f64(&self) -> f64 {
    self.source.next_f64()
  }

  fn next_range(&self, range: Range<u64>) -> u64 {
    self.source.next_range(range)
  }
}

impl Default for Random {
  /// [`Random::live`].
  fn default() -> Self {
    Self::live()
  }
}

impl fmt::Debug for Random {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("Random").finish_non_exhaustive()
  }
}

// ── effect accessors ───────────────────────────────────────────────────────────────────────

/// A `u64` from the environment's [`Random`].
pub fn random_u64<E, R, I>() -> Effect<u64, E, R>
where
  R: Selector<Random, I>,
{
  need::<Random>().map(|r| r.next_u64())
}

/// An `f64` in `[0, 1)` from the environment's [`Random`].
pub fn random_f64<E, R, I>() -> Effect<f64, E, R>
where
  R: Selector<Random, I>,
{
  need::<Random>().map(|r| r.next_f64())
}

/// A value in `range` from the environment's [`Random`]; `range.start` when it is empty.
pub fn random_range<E, R, I>(range: Range<u64>) -> Effect<u64, E, R>
where
  R: Selector<Random, I>,
{
  need::<Random>().map(move |r| r.next_range(range))
}

/// `items`, shuffled with the environment's [`Random`].
pub fn random_shuffle<T, E, R, I>(mut items: Vec<T>) -> Effect<Vec<T>, E, R>
where
  T: Send + 'static,
  R: Selector<Random, I>,
{
  need::<Random>().map(move |r| {
    r.shuffle(&mut items);
    items
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn xoshiro_matches_the_reference_vector() {
    // The reference implementation from state [1, 2, 3, 4] (as in rand_xoshiro's test).
    let mut g = Xoshiro256::from_state([1, 2, 3, 4]);
    let got: Vec<u64> = (0..10).map(|_| g.next_u64()).collect();
    assert_eq!(
      got,
      [
        11_520,
        0,
        1_509_978_240,
        1_215_971_899_390_074_240,
        1_216_172_134_540_287_360,
        607_988_272_756_665_600,
        16_172_922_978_634_559_625,
        8_476_171_486_693_032_832,
        10_595_114_339_597_558_777,
        2_904_607_092_377_533_576,
      ]
    );
  }

  #[test]
  fn splitmix64_matches_the_reference_first_output() {
    let mut s = 0;
    assert_eq!(splitmix64(&mut s), 0xE220_A839_7B1D_CDAF);
  }

  #[test]
  fn unit_f64_bounds() {
    assert_eq!(unit_f64(0), 0.0);
    assert!(unit_f64(u64::MAX) < 1.0);
  }

  #[test]
  fn uniform_in_rejects_the_biased_low_products() {
    // span 3: threshold = 2^64 mod 3 = 1, so a product whose low word is 0 is redrawn.
    let mut draws = [0_u64, u64::MAX].into_iter();
    let v = uniform_in(0..3, || draws.next().unwrap_or(0));
    assert_eq!(v, 2);
    assert_eq!(uniform_in(5..5, || unreachable!()), 5);
  }

  #[test]
  fn live_generators_differ() {
    let (a, b) = (Random::live(), Random::live());
    assert_ne!(
      (0..4).map(|_| a.next_u64()).collect::<Vec<_>>(),
      (0..4).map(|_| b.next_u64()).collect::<Vec<_>>()
    );
  }
}
