//! The immutable `Schedule<Out, In>` (Effect.ts `Schedule`) and the `retry` / `repeat` family on
//! [`Effect`].
//!
//! # Model
//!
//! A [`Schedule<Out, In>`] is an immutable, cheaply cloneable *description* of a recurrence. It
//! owns no mutable state: every run starts from [`Schedule::start`] (a fresh, opaque
//! [`ScheduleState`]) and threads that state through [`Schedule::step`], which takes the current
//! clock instant and the input (the error for `retry`, the value for `repeat`) and returns a
//! [`ScheduleStep`]: the next state, an output, and either the delay before the next recurrence
//! or "done". One schedule value can drive any number of concurrent runs.
//!
//! As in Effect.ts:
//!
//! - [`Effect::retry`] runs the effect once, and after each typed failure steps the schedule
//!   with the error: on "continue" it sleeps the delay and tries again, on "done" it fails with
//!   the last error. Defects and interrupts are never retried.
//! - [`Effect::repeat`] runs the effect once, and after each success steps the schedule with the
//!   value: on "continue" it sleeps and runs again, on "done" it succeeds with the schedule's
//!   last output. A failure stops the repetition.
//! - `recurs(n)` allows `n` recurrences (so `n + 1` attempts), `exponential(base)` delays
//!   `base * 2^k` for the `k`-th recurrence (`k = 0, 1, ..`), `union` continues while either side
//!   does (shorter delay), `intersect` while both do (longer delay).
//!
//! Delays are slept on the running fiber's executor clock, so they are virtual time on
//! `TestExecutor`; a zero delay recurs without touching the timer. Durations are
//! `std::time::Duration` (Q4).
//!
//! # Re-running an effect
//!
//! An [`Effect`] is a single-shot value (its steps are `FnOnce`), so it cannot be re-run
//! as an Effect.ts effect can. `retry` / `repeat` are therefore associated functions on `Effect`
//! that take the effect's *factory* (`Fn() -> Effect`), called once per attempt and only when the
//! attempt starts: `Effect::retry(|| fetch(id), schedule)`. There are no free `retry` / `repeat`
//! functions.
//!
//! # Jitter source
//!
//! [`Schedule::jittered`] needs randomness. It draws from a small internal SplitMix64 generator
//! seeded per run by a [`JitterSource`]: by default [`JitterSource::entropy`] (std's randomly
//! keyed hasher, no new dependency); tests pass [`JitterSource::seeded`] to
//! [`Schedule::jittered_with`] for a reproducible sequence.
//! Any other seed source (for example a seeded [`Random`](crate::Random)) plugs in through
//! [`JitterSource::from_fn`].
//!
//! ```
//! use std::sync::Arc;
//! use std::sync::atomic::{AtomicU32, Ordering};
//! use std::time::Duration;
//!
//! use id_effect::Exit;
//! use id_effect::{Effect, Schedule, fail, succeed};
//!
//! let tries = Arc::new(AtomicU32::new(0));
//! let t = Arc::clone(&tries);
//! let eff: Effect<u32, &str> = Effect::retry(
//!   move || {
//!     let n = t.fetch_add(1, Ordering::SeqCst);
//!     if n < 2 { fail("boom") } else { succeed(n) }
//!   },
//!   Schedule::exponential(Duration::from_millis(1)).intersect(Schedule::recurs(3)),
//! );
//! assert!(matches!(eff.block_on(), Exit::Success(2)));
//! assert_eq!(tries.load(Ordering::SeqCst), 3);
//! ```

use std::any::Any;
use std::fmt;
use std::future::ready;
use std::hash::{BuildHasher, RandomState};
use std::marker::PhantomData;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use crate::kernel::{Ctx, Effect, fail, sleep, succeed, suspend};

// ── state and step ──────────────────────────────────────────────────────────────────────────

/// The opaque per-run state of a [`Schedule`], created by [`Schedule::start`] and threaded
/// through [`Schedule::step`]. It belongs to the schedule that created it.
pub struct ScheduleState(Box<dyn Any + Send>);

impl ScheduleState {
  fn new<S: Send + 'static>(s: S) -> Self {
    Self(Box::new(s))
  }

  fn take<S: 'static>(self) -> S {
    match self.0.downcast::<S>() {
      Ok(s) => *s,
      Err(_) => panic!("ScheduleState stepped by a schedule that did not create it"),
    }
  }
}

impl fmt::Debug for ScheduleState {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("ScheduleState(..)")
  }
}

/// One step of a [`Schedule`]: the next state, the output, and the delay before the next
/// recurrence (`None` when the schedule is done).
#[derive(Debug)]
pub struct ScheduleStep<Out> {
  state: ScheduleState,
  out: Out,
  delay: Option<Duration>,
}

impl<Out> ScheduleStep<Out> {
  /// The output of this step.
  pub fn out(&self) -> &Out {
    &self.out
  }

  /// The delay before the next recurrence, or `None` when the schedule is done.
  pub fn delay(&self) -> Option<Duration> {
    self.delay
  }

  /// `true` when the schedule is done (no further recurrence).
  pub fn is_done(&self) -> bool {
    self.delay.is_none()
  }

  /// Splits the step into `(next state, output, delay)`.
  pub fn into_parts(self) -> (ScheduleState, Out, Option<Duration>) {
    (self.state, self.out, self.delay)
  }
}

// ── the schedule ────────────────────────────────────────────────────────────────────────────

trait Driver<Out, In>: Send + Sync {
  fn initial(&self) -> ScheduleState;
  fn step(&self, now: Instant, input: &In, state: ScheduleState) -> ScheduleStep<Out>;
}

struct FromFn<I, F, S> {
  init: I,
  step: F,
  _state: PhantomData<fn() -> S>,
}

impl<Out, In, S, I, F> Driver<Out, In> for FromFn<I, F, S>
where
  S: Send + 'static,
  I: Fn() -> S + Send + Sync,
  F: Fn(Instant, &In, S) -> (S, Out, Option<Duration>) + Send + Sync,
{
  fn initial(&self) -> ScheduleState {
    ScheduleState::new((self.init)())
  }

  fn step(&self, now: Instant, input: &In, state: ScheduleState) -> ScheduleStep<Out> {
    let (state, out, delay) = (self.step)(now, input, state.take::<S>());
    ScheduleStep {
      state: ScheduleState::new(state),
      out,
      delay,
    }
  }
}

/// An immutable recurrence policy consuming inputs of type `In` and producing outputs of type
/// `Out` (Effect.ts `Schedule<Out, In>`). See the [module docs](self) for the model.
///
/// Cloning is cheap (one `Arc`); a schedule holds no per-run state, so one value can drive many
/// runs at once.
pub struct Schedule<Out, In> {
  driver: Arc<dyn Driver<Out, In>>,
  /// `false` when no step reads its `now` (built only from the clock-free constructors and
  /// combinators): `retry` / `repeat` then step it without reading the fiber's clock.
  clock: bool,
}

impl<Out, In> Clone for Schedule<Out, In> {
  fn clone(&self) -> Self {
    Self {
      driver: Arc::clone(&self.driver),
      clock: self.clock,
    }
  }
}

impl<Out, In> fmt::Debug for Schedule<Out, In> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("Schedule(..)")
  }
}

impl<Out: 'static, In: 'static> Schedule<Out, In> {
  /// A schedule from an initial-state factory and a pure step function
  /// `(now, input, state) -> (next state, output, delay or None when done)`.
  pub fn from_fn<S, I, F>(init: I, step: F) -> Self
  where
    S: Send + 'static,
    I: Fn() -> S + Send + Sync + 'static,
    F: Fn(Instant, &In, S) -> (S, Out, Option<Duration>) + Send + Sync + 'static,
  {
    Self::with_clock(true, init, step)
  }

  /// [`from_fn`](Self::from_fn) whose `step` reads `now` only if `clock`.
  fn with_clock<S, I, F>(clock: bool, init: I, step: F) -> Self
  where
    S: Send + 'static,
    I: Fn() -> S + Send + Sync + 'static,
    F: Fn(Instant, &In, S) -> (S, Out, Option<Duration>) + Send + Sync + 'static,
  {
    Self {
      driver: Arc::new(FromFn {
        init,
        step,
        _state: PhantomData,
      }),
      clock,
    }
  }

  /// A fresh state for one run of this schedule.
  pub fn start(&self) -> ScheduleState {
    self.driver.initial()
  }

  /// Steps this schedule at clock instant `now` with `input`.
  ///
  /// # Panics
  ///
  /// If `state` was not created by this schedule (by [`start`](Self::start) or a previous
  /// `step`).
  pub fn step(&self, now: Instant, input: &In, state: ScheduleState) -> ScheduleStep<Out> {
    self.driver.step(now, input, state)
  }

  /// Transforms every output with `f` (the delays are unchanged).
  pub fn map<O2: 'static>(self, f: impl Fn(Out) -> O2 + Send + Sync + 'static) -> Schedule<O2, In> {
    let first = self.clone();
    Schedule::with_clock(
      self.clock,
      move || first.start(),
      move |now, input, s| {
        let (s, out, delay) = self.step(now, input, s).into_parts();
        (s, f(out), delay)
      },
    )
  }

  /// Continues while **either** schedule continues, with the shorter delay; outputs both
  /// outputs.
  pub fn union<O2: 'static>(self, other: Schedule<O2, In>) -> Schedule<(Out, O2), In> {
    self.zip_with(other, |a, b| match (a, b) {
      (Some(a), Some(b)) => Some(a.min(b)),
      (a, None) => a,
      (None, b) => b,
    })
  }

  /// Continues while **both** schedules continue, with the longer delay; outputs both outputs.
  pub fn intersect<O2: 'static>(self, other: Schedule<O2, In>) -> Schedule<(Out, O2), In> {
    self.zip_with(other, |a, b| match (a, b) {
      (Some(a), Some(b)) => Some(a.max(b)),
      _ => None,
    })
  }

  fn zip_with<O2: 'static>(
    self,
    other: Schedule<O2, In>,
    combine: fn(Option<Duration>, Option<Duration>) -> Option<Duration>,
  ) -> Schedule<(Out, O2), In> {
    let (l, r) = (self.clone(), other.clone());
    Schedule::with_clock(
      self.clock || other.clock,
      move || (l.start(), r.start()),
      move |now, input, (sa, sb)| {
        let (sa, a, da) = self.step(now, input, sa).into_parts();
        let (sb, b, db) = other.step(now, input, sb).into_parts();
        ((sa, sb), (a, b), combine(da, db))
      },
    )
  }

  /// Runs this schedule until it is done, then `other` (stepped at once with the same input).
  pub fn and_then(self, other: Schedule<Out, In>) -> Self {
    enum Phase {
      First(ScheduleState),
      Second(ScheduleState),
    }
    let first = self.clone();
    Schedule::with_clock(
      self.clock || other.clock,
      move || Phase::First(first.start()),
      move |now, input, phase| {
        if let Phase::First(s) = phase {
          let (s, out, delay) = self.step(now, input, s).into_parts();
          if delay.is_some() {
            return (Phase::First(s), out, delay);
          }
          let (s, out, delay) = other.step(now, input, other.start()).into_parts();
          return (Phase::Second(s), out, delay);
        }
        let Phase::Second(s) = phase else {
          unreachable!("the first phase returned above")
        };
        let (s, out, delay) = other.step(now, input, s).into_parts();
        (Phase::Second(s), out, delay)
      },
    )
  }

  /// Continues only while `pred` holds for the input (Effect.ts `whileInput`).
  pub fn while_input(self, pred: impl Fn(&In) -> bool + Send + Sync + 'static) -> Self {
    let first = self.clone();
    Schedule::with_clock(
      self.clock,
      move || first.start(),
      move |now, input, s| {
        let (s, out, delay) = self.step(now, input, s).into_parts();
        (s, out, delay.filter(|_| pred(input)))
      },
    )
  }

  /// Continues only while less than `max` has elapsed since the first step (Effect.ts `upTo`).
  pub fn up_to(self, max: Duration) -> Self {
    let first = self.clone();
    Schedule::from_fn(
      move || (first.start(), None::<Instant>),
      move |now, input, (s, start)| {
        let start = start.unwrap_or(now);
        let (s, out, delay) = self.step(now, input, s).into_parts();
        let delay = delay.filter(|_| now.saturating_duration_since(start) < max);
        ((s, Some(start)), out, delay)
      },
    )
  }

  /// Scales every delay by a random factor in `[0.8, 1.2]` (Effect.ts `jittered`), drawn from
  /// [`JitterSource::entropy`].
  pub fn jittered(self) -> Self {
    self.jittered_with(0.8, 1.2, JitterSource::entropy())
  }

  /// Scales every delay by a random factor in `[min, max]`, drawn from a generator seeded by
  /// `source` once per run. A factor that is not positive gives a zero delay; one too large to
  /// represent gives `Duration::MAX`.
  pub fn jittered_with(self, min: f64, max: f64, source: JitterSource) -> Self {
    let first = self.clone();
    Schedule::with_clock(
      self.clock,
      move || (first.start(), (source.seed)()),
      move |now, input, (s, mut rng)| {
        let (s, out, delay) = self.step(now, input, s).into_parts();
        let delay = delay.map(|d| {
          let factor = min + (max - min) * unit_f64(splitmix64(&mut rng));
          scale(d, factor)
        });
        ((s, rng), out, delay)
      },
    )
  }
}

impl<In: 'static> Schedule<u64, In> {
  /// Recurs forever with no delay; outputs the recurrence count (`0, 1, 2, ..`).
  pub fn forever() -> Self {
    Self::with_clock(
      false,
      || 0u64,
      |_, _, n| (n.saturating_add(1), n, Some(Duration::ZERO)),
    )
  }

  /// Recurs `times` times with no delay; outputs the recurrence count.
  pub fn recurs(times: u64) -> Self {
    Self::with_clock(
      false,
      || 0u64,
      move |_, _, n| {
        (
          n.saturating_add(1),
          n,
          (n < times).then_some(Duration::ZERO),
        )
      },
    )
  }

  /// Recurs forever, `interval` apart; outputs the recurrence count.
  pub fn spaced(interval: Duration) -> Self {
    Self::with_clock(
      false,
      || 0u64,
      move |_, _, n| (n.saturating_add(1), n, Some(interval)),
    )
  }
}

impl<In: 'static> Schedule<Duration, In> {
  /// Recurs forever with delays `base * 2^k` (`base`, `2 * base`, `4 * base`, ..); outputs the
  /// delay. Saturates at `Duration::MAX`.
  pub fn exponential(base: Duration) -> Self {
    Self::with_clock(
      false,
      || 0u32,
      move |_, _, k| {
        let d = 1u32
          .checked_shl(k)
          .map_or(Duration::MAX, |m| base.saturating_mul(m));
        (k.saturating_add(1), d, Some(d))
      },
    )
  }

  /// Recurs forever with delays `base * factor^k`; outputs the delay (Effect.ts
  /// `exponential(base, factor)`).
  pub fn exponential_by(base: Duration, factor: f64) -> Self {
    Self::with_clock(
      false,
      || 0i32,
      move |_, _, k| {
        let d = scale(base, factor.powi(k));
        (k.saturating_add(1), d, Some(d))
      },
    )
  }

  /// Recurs forever with Fibonacci delays (`one`, `one`, `2 * one`, `3 * one`, `5 * one`, ..);
  /// outputs the delay.
  pub fn fibonacci(one: Duration) -> Self {
    Self::with_clock(
      false,
      move || (one, one),
      |_, _, (a, b)| ((b, a.saturating_add(b)), a, Some(a)),
    )
  }
}

/// `d * factor`, saturating: not positive gives zero, unrepresentable gives `Duration::MAX`.
fn scale(d: Duration, factor: f64) -> Duration {
  if factor.is_nan() || factor <= 0.0 {
    return Duration::ZERO;
  }
  Duration::try_from_secs_f64(d.as_secs_f64() * factor).unwrap_or(Duration::MAX)
}

// ── jitter source ───────────────────────────────────────────────────────────────────────────

/// Seeds the generator behind [`Schedule::jittered_with`], once per run (see the
/// [module docs](self)).
#[derive(Clone)]
pub struct JitterSource {
  seed: Arc<dyn Fn() -> u64 + Send + Sync>,
}

impl fmt::Debug for JitterSource {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("JitterSource(..)")
  }
}

impl JitterSource {
  /// A fresh seed per run from std's randomly keyed hasher (no new dependency).
  pub fn entropy() -> Self {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    Self::from_fn(|| RandomState::new().hash_one(COUNTER.fetch_add(1, Ordering::Relaxed)))
  }

  /// The same seed for every run: a reproducible jitter sequence (for tests).
  pub fn seeded(seed: u64) -> Self {
    Self::from_fn(move || seed)
  }

  /// Seeds from `f`, called once per run.
  pub fn from_fn(f: impl Fn() -> u64 + Send + Sync + 'static) -> Self {
    Self { seed: Arc::new(f) }
  }
}

/// SplitMix64: advances `state` and returns the next output.
fn splitmix64(state: &mut u64) -> u64 {
  *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
  let mut z = *state;
  z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
  z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
  z ^ (z >> 31)
}

/// The top 53 bits of `x` as a float in `[0, 1)`.
fn unit_f64(x: u64) -> f64 {
  (x >> 11) as f64 / (1u64 << 53) as f64
}

// ── retry / repeat ──────────────────────────────────────────────────────────────────────────

/// The running fiber's executor clock (virtual on `TestExecutor`).
fn clock_now<E: Send + 'static, R: Send + Sync + 'static>() -> Effect<Instant, E, R> {
  Effect::from_ctx(|ctx: Ctx<R>| ready(Ok(ctx.fiber().executor().now())))
}

/// `f(now)` for a step of a schedule: the fiber's clock when the schedule reads it (`clock`),
/// else at once with a fixed instant it ignores, so a retry or repeat of a synchronous effect
/// under a clock-free schedule needs no fiber (and settles inline in `block_on`).
fn with_now<A, E, R>(
  clock: bool,
  f: impl FnOnce(Instant) -> Effect<A, E, R> + Send + 'static,
) -> Effect<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  static UNREAD: OnceLock<Instant> = OnceLock::new();
  if clock {
    clock_now().flat_map(f)
  } else {
    f(*UNREAD.get_or_init(Instant::now))
  }
}

/// `next` after sleeping `d` (no timer for a zero delay).
fn after<A, E, R>(d: Duration, next: Effect<A, E, R>) -> Effect<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  if d.is_zero() {
    next
  } else {
    sleep(d).then(next)
  }
}

// Each attempt is built (`make()`) when it starts, inside one `suspend`, and its recovery is
// attached to the built effect: a ready attempt (`succeed` / `fail`) then stays a typed
// synchronous step instead of an erased `Fold` around a deferred one.

fn retry_loop<A, E, E2, R, Out, F, G>(
  make: Arc<F>,
  schedule: Schedule<Out, E>,
  state: ScheduleState,
  or_else: G,
) -> Effect<A, E2, R>
where
  A: Send + 'static,
  E: Send + 'static,
  E2: Send + 'static,
  R: Send + Sync + 'static,
  Out: Send + 'static,
  F: Fn() -> Effect<A, E, R> + Send + Sync + 'static,
  G: FnOnce(E, Out) -> Effect<A, E2, R> + Send + 'static,
{
  suspend(move || {
    make().catch(move |e| {
      with_now(schedule.clock, move |now| {
        let (state, out, delay) = schedule.step(now, &e, state).into_parts();
        match delay {
          Some(d) => after(d, retry_loop(make, schedule, state, or_else)),
          None => or_else(e, out),
        }
      })
    })
  })
}

fn repeat_loop<A, E, E2, R, Out, F, G>(
  make: Arc<F>,
  schedule: Schedule<Out, A>,
  state: ScheduleState,
  last: Option<Out>,
  or_else: G,
) -> Effect<Out, E2, R>
where
  A: Send + 'static,
  E: Send + 'static,
  E2: Send + 'static,
  R: Send + Sync + 'static,
  Out: Send + 'static,
  F: Fn() -> Effect<A, E, R> + Send + Sync + 'static,
  G: FnOnce(E, Option<Out>) -> Effect<Out, E2, R> + Send + 'static,
{
  suspend(move || {
    make()
      .map(Ok)
      .catch(|e| succeed(Err(e)))
      .flat_map(move |r: Result<A, E>| match r {
        Err(e) => or_else(e, last),
        Ok(a) => with_now(schedule.clock, move |now| {
          let (state, out, delay) = schedule.step(now, &a, state).into_parts();
          match delay {
            Some(d) => after(d, repeat_loop(make, schedule, state, Some(out), or_else)),
            None => succeed(out),
          }
        }),
      })
  })
}

impl<A, E, R> Effect<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  /// Runs `make()` and retries it on typed failure as `schedule` (stepped with each error)
  /// allows; fails with the last error once the schedule is done. Defects and interrupts are not
  /// retried. `make` is called once per attempt, when the attempt starts.
  pub fn retry<Out: Send + 'static>(
    make: impl Fn() -> Self + Send + Sync + 'static,
    schedule: Schedule<Out, E>,
  ) -> Self {
    Self::retry_or_else(make, schedule, |e, _| fail(e))
  }

  /// [`retry`](Self::retry), recovering with `or_else(last error, schedule's last output)` once
  /// the schedule is done.
  pub fn retry_or_else<Out: Send + 'static, E2: Send + 'static>(
    make: impl Fn() -> Self + Send + Sync + 'static,
    schedule: Schedule<Out, E>,
    or_else: impl FnOnce(E, Out) -> Effect<A, E2, R> + Send + 'static,
  ) -> Effect<A, E2, R> {
    let make = Arc::new(make);
    suspend(move || {
      let state = schedule.start();
      retry_loop(make, schedule, state, or_else)
    })
  }

  /// Runs `make()` and runs it again after each success as `schedule` (stepped with each value)
  /// allows; succeeds with the schedule's last output once it is done. The first failure stops
  /// the repetition and is returned.
  pub fn repeat<Out: Send + 'static>(
    make: impl Fn() -> Self + Send + Sync + 'static,
    schedule: Schedule<Out, A>,
  ) -> Effect<Out, E, R> {
    Self::repeat_or_else(make, schedule, |e, _| fail(e))
  }

  /// [`repeat`](Self::repeat), recovering from a failure with `or_else(error, schedule's last
  /// output so far)` (`None` when the first run failed).
  pub fn repeat_or_else<Out: Send + 'static, E2: Send + 'static>(
    make: impl Fn() -> Self + Send + Sync + 'static,
    schedule: Schedule<Out, A>,
    or_else: impl FnOnce(E, Option<Out>) -> Effect<Out, E2, R> + Send + 'static,
  ) -> Effect<Out, E2, R> {
    let make = Arc::new(make);
    suspend(move || {
      let state = schedule.start();
      repeat_loop(make, schedule, state, None, or_else)
    })
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn delays<Out: 'static>(s: &Schedule<Out, ()>, n: usize) -> Vec<Option<Duration>> {
    let now = Instant::now();
    let mut state = s.start();
    let mut out = Vec::new();
    for _ in 0..n {
      let (next, _, d) = s.step(now, &(), state).into_parts();
      state = next;
      out.push(d);
    }
    out
  }

  fn ms(n: u64) -> Option<Duration> {
    Some(Duration::from_millis(n))
  }

  #[test]
  fn exponential_saturates() {
    let s = Schedule::<Duration, ()>::exponential(Duration::from_secs(1));
    assert_eq!(delays(&s, 40)[39], Some(Duration::MAX));
  }

  #[test]
  fn exponential_by_and_scale_edges() {
    let s = Schedule::<Duration, ()>::exponential_by(Duration::from_millis(10), 3.0);
    assert_eq!(delays(&s, 3), vec![ms(10), ms(30), ms(90)]);
    assert_eq!(scale(Duration::from_secs(1), -1.0), Duration::ZERO);
    assert_eq!(scale(Duration::from_secs(1), f64::NAN), Duration::ZERO);
    assert_eq!(scale(Duration::MAX, 1e300), Duration::MAX);
  }

  #[test]
  #[should_panic(expected = "did not create it")]
  fn foreign_state_panics() {
    let a = Schedule::<u64, ()>::forever();
    let b = Schedule::<Duration, ()>::fibonacci(Duration::from_millis(1));
    let _ = a.step(Instant::now(), &(), b.start());
  }

  #[test]
  fn debug_impls() {
    let s = Schedule::<u64, ()>::forever();
    assert_eq!(format!("{s:?}"), "Schedule(..)");
    assert_eq!(format!("{:?}", s.start()), "ScheduleState(..)");
    assert_eq!(format!("{:?}", JitterSource::seeded(1)), "JitterSource(..)");
  }

  #[test]
  fn unit_f64_is_in_range() {
    assert_eq!(unit_f64(0), 0.0);
    assert!(unit_f64(u64::MAX) < 1.0);
  }
}
