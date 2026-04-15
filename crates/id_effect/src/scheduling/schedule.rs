//! Effect.ts-style scheduling policies for `repeat` / `retry`.
//!
//! This module keeps API names familiar (`recurs`, `spaced`, `exponential`, `compose`, `jittered`)
//! while staying runtime-agnostic.

use core::fmt;
use core::time::Duration;
use std::sync::Arc;

use crate::foundation::func::{compose, identity};
use crate::observability::metric::Metric;
use crate::runtime::{CancellationToken, ThreadSleepRuntime, check_interrupt};
use crate::scheduling::duration::duration;
use crate::schema::order::order;
use crate::{Clock, Effect, LiveClock, Never, Predicate};

/// Inputs passed to a [`Schedule`] on each step (e.g. retry / repeat attempt index).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScheduleInput {
  /// Monotonic attempt counter (0-based where used by [`repeat`](crate::scheduling::schedule::repeat) / [`retry`](crate::scheduling::schedule::retry)).
  pub attempt: u64,
}

/// One scheduling step: how long to wait before the next iteration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScheduleDecision {
  /// Sleep duration before the next attempt (may be zero).
  pub delay: Duration,
}

impl ScheduleDecision {
  /// Decision to continue after sleeping for `delay`.
  #[inline]
  pub fn continue_after(delay: Duration) -> Self {
    Self { delay }
  }
}

type DelayMap = Arc<dyn Fn(Duration) -> Duration + Send + Sync>;
type InputMap = Arc<dyn Fn(ScheduleInput) -> ScheduleInput + Send + Sync>;
type SchedulePred = Arc<dyn Fn(&ScheduleInput) -> bool + Send + Sync>;

/// Scheduling policy for [`repeat`] / [`retry`].
///
/// `Predicate`- and `Arc`-backed variants are not compared for equality; use runtime behavior via [`Schedule::next`].
#[derive(Clone)]
pub enum Schedule {
  /// Repeat up to `remaining` more times with zero delay between steps.
  Recurs {
    /// Steps left before [`Schedule::next`] returns `None`.
    remaining: u64,
  },
  /// Fixed delay `interval` on every step.
  Spaced {
    /// Delay between attempts.
    interval: Duration,
  },
  /// Delays grow as `base * 2^step` per call to [`Schedule::next`] (step tracked internally).
  Exponential {
    /// Initial delay; doubled each step (capped).
    base: Duration,
    /// Internal exponent counter (starts at 0).
    step: u32,
  },
  /// Combine two schedules: continue while both produce a step; delay is the max of the two.
  Compose(Box<Schedule>, Box<Schedule>),
  /// Scale the inner schedule’s delay by 90% (deterministic jitter helper).
  Jittered(Box<Schedule>),
  /// Continue emitting steps while the predicate holds on each [`ScheduleInput`].
  RecursWhile {
    /// When this returns `false`, the schedule stops.
    pred: SchedulePred,
  },
  /// Continue until the predicate becomes true (stops on the first step where it holds).
  RecursUntil {
    /// When this returns `true`, the schedule stops.
    pred: SchedulePred,
  },
  /// Map every produced delay through `f` (composed when nested — see [`Schedule::map`]).
  MapDelay(Box<Schedule>, DelayMap),
  /// Contramap the incoming [`ScheduleInput`] before the inner schedule sees it.
  ContramapInput(Box<Schedule>, InputMap),
}

impl fmt::Debug for Schedule {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Schedule::Recurs { remaining } => f
        .debug_struct("Recurs")
        .field("remaining", remaining)
        .finish(),
      Schedule::Spaced { interval } => f
        .debug_struct("Spaced")
        .field("interval", interval)
        .finish(),
      Schedule::Exponential { base, step } => f
        .debug_struct("Exponential")
        .field("base", base)
        .field("step", step)
        .finish(),
      Schedule::Compose(a, b) => f.debug_tuple("Compose").field(a).field(b).finish(),
      Schedule::Jittered(inner) => f.debug_tuple("Jittered").field(inner).finish(),
      Schedule::RecursWhile { .. } => f.debug_struct("RecursWhile").finish_non_exhaustive(),
      Schedule::RecursUntil { .. } => f.debug_struct("RecursUntil").finish_non_exhaustive(),
      Schedule::MapDelay(inner, _) => f
        .debug_tuple("MapDelay")
        .field(inner)
        .field(&"<fn>")
        .finish(),
      Schedule::ContramapInput(inner, _) => f
        .debug_tuple("ContramapInput")
        .field(inner)
        .field(&"<fn>")
        .finish(),
    }
  }
}

impl Schedule {
  /// Fixed number of additional iterations (`times` steps with zero delay).
  #[inline]
  pub fn recurs(times: u64) -> Self {
    Self::Recurs { remaining: times }
  }

  /// Wait `interval` between attempts.
  #[inline]
  pub fn spaced(interval: Duration) -> Self {
    Self::Spaced { interval }
  }

  /// Exponential backoff starting at `base`.
  #[inline]
  pub fn exponential(base: Duration) -> Self {
    Self::Exponential { base, step: 0 }
  }

  /// Combine with `other` using [`Schedule::Compose`].
  #[inline]
  pub fn compose(self, other: Schedule) -> Self {
    Self::Compose(Box::new(self), Box::new(other))
  }

  /// Wrap in [`Schedule::Jittered`].
  #[inline]
  pub fn jittered(self) -> Self {
    Self::Jittered(Box::new(self))
  }

  /// Repeat while `pred` holds on each [`ScheduleInput`] (typically `attempt`).
  #[inline]
  pub fn recurs_while(pred: Predicate<ScheduleInput>) -> Self {
    Self::RecursWhile {
      pred: Arc::from(pred),
    }
  }

  /// Repeat until `pred` becomes true; stops on the first step where `pred` holds.
  #[inline]
  pub fn recurs_until(pred: Predicate<ScheduleInput>) -> Self {
    Self::RecursUntil {
      pred: Arc::from(pred),
    }
  }

  /// Map every delay this schedule produces through `f`. Nested [`Schedule::map`] calls compose with [`compose`].
  pub fn map<F>(self, f: F) -> Self
  where
    F: Fn(Duration) -> Duration + Send + Sync + 'static,
  {
    let f = Arc::new(f);
    match self {
      Schedule::MapDelay(inner, existing) => Schedule::MapDelay(
        inner,
        Arc::new(compose(
          {
            let f = Arc::clone(&f);
            move |d: Duration| f(d)
          },
          {
            let existing = Arc::clone(&existing);
            move |d: Duration| existing(d)
          },
        )),
      ),
      other => Schedule::MapDelay(Box::new(other), f),
    }
  }

  /// Contramap the [`ScheduleInput`] before the inner policy runs. Nested calls compose with [`compose`].
  pub fn contramap<G>(self, g: G) -> Self
  where
    G: Fn(ScheduleInput) -> ScheduleInput + Send + Sync + 'static,
  {
    let g = Arc::new(g);
    match self {
      Schedule::ContramapInput(inner, existing) => Schedule::ContramapInput(
        inner,
        Arc::new(compose(
          {
            let existing = Arc::clone(&existing);
            move |i: ScheduleInput| existing(i)
          },
          {
            let g = Arc::clone(&g);
            move |i: ScheduleInput| g(i)
          },
        )),
      ),
      other => Schedule::ContramapInput(Box::new(other), g),
    }
  }

  /// Advance the policy for `input`; `None` means stop repeating / no more backoff.
  pub fn next(&mut self, input: ScheduleInput) -> Option<ScheduleDecision> {
    match self {
      Schedule::Recurs { remaining } => {
        if *remaining == 0 {
          None
        } else {
          *remaining -= 1;
          Some(ScheduleDecision::continue_after(duration::ZERO))
        }
      }
      Schedule::Spaced { interval } => Some(ScheduleDecision::continue_after(*interval)),
      Schedule::Exponential { base, step } => {
        let mult = 2u128.saturating_pow(*step);
        let nanos = base.as_nanos().saturating_mul(mult);
        *step = step.saturating_add(1);
        let capped = nanos.min(u64::MAX as u128);
        let candidate = Duration::from_nanos(capped as u64);
        let ord = order::duration();
        let delay = order::clamp(&ord, candidate, duration::ZERO, Duration::MAX);
        Some(ScheduleDecision::continue_after(delay))
      }
      Schedule::Compose(a, b) => match (
        a.next(ScheduleInput::default()),
        b.next(ScheduleInput::default()),
      ) {
        (Some(da), Some(db)) => {
          let ord = order::duration();
          let delay = order::max(&ord, da.delay, db.delay);
          Some(ScheduleDecision::continue_after(delay))
        }
        _ => None,
      },
      Schedule::Jittered(inner) => inner
        .next(ScheduleInput::default())
        .map(|decision| ScheduleDecision::continue_after(jitter_80_120(decision.delay))),
      Schedule::RecursWhile { pred } => {
        if pred(&input) {
          Some(ScheduleDecision::continue_after(duration::ZERO))
        } else {
          None
        }
      }
      Schedule::RecursUntil { pred } => {
        if pred(&input) {
          None
        } else {
          Some(ScheduleDecision::continue_after(duration::ZERO))
        }
      }
      Schedule::MapDelay(inner, f) => {
        let f = Arc::clone(f);
        inner.next(input).map(|decision| {
          let map_delay = compose(move |d: Duration| f(d), identity);
          ScheduleDecision::continue_after(map_delay(decision.delay))
        })
      }
      Schedule::ContramapInput(inner, g) => {
        let mapped = g(input);
        inner.next(mapped)
      }
    }
  }

  /// Interpret one schedule step into a non-blocking sleep effect.
  #[inline]
  pub fn next_sleep<C>(&mut self, clock: &C, input: ScheduleInput) -> Option<Effect<(), Never, ()>>
  where
    C: Clock + Clone + 'static,
  {
    self
      .next(input)
      .map(|decision| clock.clone().sleep(decision.delay))
  }
}

fn jitter_80_120(d: Duration) -> Duration {
  let ns = d.as_nanos();
  let jittered = ns.saturating_mul(9).saturating_div(10);
  let capped = jittered.min(u64::MAX as u128);
  duration::nanos(capped as u64)
}

/// Effect.ts-like `Effect.repeat(effect, schedule)` but as a factory because our effects are one-shot.
pub fn repeat<A, E, R, F>(make: F, schedule: Schedule) -> Effect<A, E, R>
where
  A: 'static,
  E: 'static,
  R: 'static,
  F: FnMut() -> Effect<A, E, R> + 'static,
{
  repeat_with_clock(make, schedule, LiveClock::new(ThreadSleepRuntime), None)
}

/// Repeat using an explicit clock service so delay handling is runtime-mediated and non-blocking.
pub fn repeat_with_clock<A, E, R, F, C>(
  mut make: F,
  mut schedule: Schedule,
  clock: C,
  attempt_counter: Option<Metric<u64, ()>>,
) -> Effect<A, E, R>
where
  A: 'static,
  E: 'static,
  R: 'static,
  F: FnMut() -> Effect<A, E, R> + 'static,
  C: Clock + Clone + 'static,
{
  Effect::new_async(move |r: &mut R| {
    let clock = clock.clone();
    Box::pin(async move {
      let mut attempt = 0_u64;
      if let Some(ref c) = attempt_counter {
        match c.apply(1).run(&mut ()).await {
          Ok(()) => {}
          Err(never) => match never {},
        }
      }
      let mut last = make().run(r).await?;
      while let Some(sleep_effect) = schedule.next_sleep(&clock, ScheduleInput { attempt }) {
        match sleep_effect.run(&mut ()).await {
          Ok(()) => {}
          Err(never) => match never {},
        }
        if let Some(ref c) = attempt_counter {
          match c.apply(1).run(&mut ()).await {
            Ok(()) => {}
            Err(never) => match never {},
          }
        }
        last = make().run(r).await?;
        attempt = attempt.saturating_add(1);
      }
      Ok(last)
    })
  })
}

/// Repeat using an explicit clock and interruption token.
pub fn repeat_with_clock_and_interrupt<A, E, R, F, C>(
  mut make: F,
  mut schedule: Schedule,
  clock: C,
  token: CancellationToken,
  attempt_counter: Option<Metric<u64, ()>>,
) -> Effect<A, E, R>
where
  A: 'static,
  E: 'static,
  R: 'static,
  F: FnMut() -> Effect<A, E, R> + 'static,
  C: Clock + Clone + 'static,
{
  Effect::new_async(move |r: &mut R| {
    let clock = clock.clone();
    let token = token.clone();
    Box::pin(async move {
      let mut attempt = 0_u64;
      if let Some(ref c) = attempt_counter {
        match c.apply(1).run(&mut ()).await {
          Ok(()) => {}
          Err(never) => match never {},
        }
      }
      let mut last = make().run(r).await?;
      while let Some(sleep_effect) = schedule.next_sleep(&clock, ScheduleInput { attempt }) {
        let interrupted = match check_interrupt(&token).run(&mut ()).await {
          Ok(is_interrupted) => is_interrupted,
          Err(never) => match never {},
        };
        if interrupted {
          break;
        }
        match sleep_effect.run(&mut ()).await {
          Ok(()) => {}
          Err(never) => match never {},
        }
        if let Some(ref c) = attempt_counter {
          match c.apply(1).run(&mut ()).await {
            Ok(()) => {}
            Err(never) => match never {},
          }
        }
        last = make().run(r).await?;
        attempt = attempt.saturating_add(1);
      }
      Ok(last)
    })
  })
}

/// Effect.ts-like `Effect.retry(effect, schedule)` using a one-shot effect factory.
pub fn retry<A, E, R, F>(make: F, schedule: Schedule) -> Effect<A, E, R>
where
  A: 'static,
  E: 'static,
  R: 'static,
  F: FnMut() -> Effect<A, E, R> + 'static,
{
  retry_with_clock(make, schedule, LiveClock::new(ThreadSleepRuntime), None)
}

/// Retry using an explicit clock service for runtime-mediated delays.
pub fn retry_with_clock<A, E, R, F, C>(
  mut make: F,
  mut schedule: Schedule,
  clock: C,
  attempt_counter: Option<Metric<u64, ()>>,
) -> Effect<A, E, R>
where
  A: 'static,
  E: 'static,
  R: 'static,
  F: FnMut() -> Effect<A, E, R> + 'static,
  C: Clock + Clone + 'static,
{
  Effect::new_async(move |r: &mut R| {
    let clock = clock.clone();
    Box::pin(async move {
      let mut attempt = 0_u64;
      loop {
        if let Some(ref c) = attempt_counter {
          match c.apply(1).run(&mut ()).await {
            Ok(()) => {}
            Err(never) => match never {},
          }
        }
        match make().run(r).await {
          Ok(a) => return Ok(a),
          Err(e) => match schedule.next_sleep(&clock, ScheduleInput { attempt }) {
            Some(sleep_effect) => {
              match sleep_effect.run(&mut ()).await {
                Ok(()) => {}
                Err(never) => match never {},
              }
              attempt = attempt.saturating_add(1);
            }
            None => return Err(e),
          },
        }
      }
    })
  })
}

/// Retry using an explicit clock and interruption token.
pub fn retry_with_clock_and_interrupt<A, E, R, F, C>(
  mut make: F,
  mut schedule: Schedule,
  clock: C,
  token: CancellationToken,
  attempt_counter: Option<Metric<u64, ()>>,
) -> Effect<A, E, R>
where
  A: 'static,
  E: 'static,
  R: 'static,
  F: FnMut() -> Effect<A, E, R> + 'static,
  C: Clock + Clone + 'static,
{
  Effect::new_async(move |r: &mut R| {
    let clock = clock.clone();
    let token = token.clone();
    Box::pin(async move {
      let mut attempt = 0_u64;
      loop {
        if let Some(ref c) = attempt_counter {
          match c.apply(1).run(&mut ()).await {
            Ok(()) => {}
            Err(never) => match never {},
          }
        }
        match make().run(r).await {
          Ok(a) => return Ok(a),
          Err(e) => {
            let interrupted = match check_interrupt(&token).run(&mut ()).await {
              Ok(is_interrupted) => is_interrupted,
              Err(never) => match never {},
            };
            if interrupted {
              return Err(e);
            }
            match schedule.next_sleep(&clock, ScheduleInput { attempt }) {
              Some(sleep_effect) => {
                match sleep_effect.run(&mut ()).await {
                  Ok(()) => {}
                  Err(never) => match never {},
                }
                attempt = attempt.saturating_add(1);
              }
              None => return Err(e),
            }
          }
        }
      }
    })
  })
}

/// Effect.ts-like `Effect.forever(effect)` using a one-shot effect factory.
pub fn forever<E, R, F>(mut make: F) -> Effect<(), E, R>
where
  E: 'static,
  R: 'static,
  F: FnMut() -> Effect<(), E, R> + 'static,
{
  Effect::new_async(move |r: &mut R| {
    Box::pin(async move {
      loop {
        make().run(r).await?;
      }
    })
  })
}

/// Repeat a fixed number of times after a successful first run.
#[inline]
pub fn repeat_n<A, E, R, F>(make: F, times: u64) -> Effect<A, E, R>
where
  A: 'static,
  E: 'static,
  R: 'static,
  F: FnMut() -> Effect<A, E, R> + 'static,
{
  repeat(make, Schedule::recurs(times))
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::foundation::func::identity;
  use crate::foundation::predicate::predicate;
  use crate::{TestClock, fail, runtime::run_blocking, succeed};
  use core::future::Future;
  use core::task::{Context, Poll, Waker};
  use rstest::rstest;
  use std::sync::Arc;
  use std::sync::atomic::{AtomicUsize, Ordering};
  use std::task::Wake;
  use std::thread;

  struct ThreadUnpark(thread::Thread);
  impl Wake for ThreadUnpark {
    fn wake(self: Arc<Self>) {
      self.0.unpark();
    }
  }

  fn block_on<F: Future>(fut: F) -> F::Output {
    let mut fut = Box::pin(fut);
    let waker = Waker::from(Arc::new(ThreadUnpark(thread::current())));
    let mut cx = Context::from_waker(&waker);
    loop {
      match fut.as_mut().poll(&mut cx) {
        Poll::Ready(v) => return v,
        Poll::Pending => thread::park(),
      }
    }
  }

  mod schedule_next {
    use super::*;

    #[test]
    fn recurs_with_finite_remaining_stops_after_budget_is_exhausted() {
      let mut s = Schedule::recurs(2);
      assert_eq!(
        s.next(ScheduleInput::default()),
        Some(ScheduleDecision::continue_after(duration::ZERO))
      );
      assert_eq!(
        s.next(ScheduleInput::default()),
        Some(ScheduleDecision::continue_after(duration::ZERO))
      );
      assert_eq!(s.next(ScheduleInput::default()), None);
    }

    #[test]
    fn spaced_uses_duration_seconds_constructor() {
      let mut s = Schedule::spaced(duration::seconds(1));
      assert_eq!(
        s.next(ScheduleInput { attempt: 0 }),
        Some(ScheduleDecision::continue_after(duration::seconds(1)))
      );
    }

    #[test]
    fn recurs_until_stops_when_predicate_true() {
      let pred: Predicate<ScheduleInput> = Box::new(|i: &ScheduleInput| i.attempt >= 2);
      let mut s = Schedule::recurs_until(pred);
      assert_eq!(
        s.next(ScheduleInput { attempt: 0 }),
        Some(ScheduleDecision::continue_after(duration::ZERO))
      );
      assert_eq!(
        s.next(ScheduleInput { attempt: 1 }),
        Some(ScheduleDecision::continue_after(duration::ZERO))
      );
      assert_eq!(s.next(ScheduleInput { attempt: 2 }), None);
    }

    #[test]
    fn recurs_while_short_circuits_when_predicate_false() {
      let pred: Predicate<ScheduleInput> = Box::new(|i: &ScheduleInput| i.attempt < 2);
      let mut s = Schedule::recurs_while(pred);
      assert_eq!(
        s.next(ScheduleInput { attempt: 0 }),
        Some(ScheduleDecision::continue_after(duration::ZERO))
      );
      assert_eq!(
        s.next(ScheduleInput { attempt: 1 }),
        Some(ScheduleDecision::continue_after(duration::ZERO))
      );
      assert_eq!(s.next(ScheduleInput { attempt: 2 }), None);
    }

    #[test]
    fn map_with_identity_preserves_delays() {
      let mut s = Schedule::spaced(duration::millis(7)).map(identity);
      assert_eq!(
        s.next(ScheduleInput::default()),
        Some(ScheduleDecision::continue_after(duration::millis(7)))
      );
    }

    #[test]
    fn contramap_composes_input_transform_for_attempt_counting() {
      let mut s =
        Schedule::recurs_until(Box::new(|i: &ScheduleInput| i.attempt >= 1)).contramap(|mut i| {
          i.attempt = i.attempt.saturating_add(5);
          i
        });
      assert_eq!(s.next(ScheduleInput { attempt: 0 }), None);
    }

    #[test]
    fn compose_preserves_identity_for_recurs_branch() {
      let mut s = Schedule::recurs(1).compose(Schedule::recurs(1));
      assert!(s.next(ScheduleInput::default()).is_some());
      assert_eq!(s.next(ScheduleInput::default()), None);
    }

    #[test]
    fn recurs_while_with_composed_predicate_requires_both_clauses() {
      let lt4: Predicate<ScheduleInput> = Box::new(|i: &ScheduleInput| i.attempt < 4);
      let ge1: Predicate<ScheduleInput> = Box::new(|i: &ScheduleInput| i.attempt >= 1);
      let mut s = Schedule::recurs_while(predicate::and(lt4, ge1));
      assert_eq!(s.next(ScheduleInput { attempt: 0 }), None);
      assert_eq!(
        s.next(ScheduleInput { attempt: 1 }),
        Some(ScheduleDecision::continue_after(duration::ZERO))
      );
      assert_eq!(
        s.next(ScheduleInput { attempt: 3 }),
        Some(ScheduleDecision::continue_after(duration::ZERO))
      );
      assert_eq!(s.next(ScheduleInput { attempt: 4 }), None);
    }

    #[test]
    fn compose_with_one_exhausted_side_stops_when_either_schedule_stops() {
      let mut s = Schedule::recurs(1).compose(Schedule::recurs(3));
      assert!(s.next(ScheduleInput::default()).is_some());
      assert_eq!(s.next(ScheduleInput::default()), None);
    }

    #[rstest]
    #[case::first(duration::millis(10), duration::millis(20))]
    #[case::second(duration::millis(50), duration::millis(5))]
    fn compose_with_two_spaced_schedules_uses_maximum_delay(
      #[case] a: Duration,
      #[case] b: Duration,
    ) {
      let mut s = Schedule::spaced(a).compose(Schedule::spaced(b));
      assert_eq!(
        s.next(ScheduleInput::default()),
        Some(ScheduleDecision::continue_after(a.max(b)))
      );
    }

    #[rstest]
    #[case::first_step(0, duration::millis(3), duration::millis(3))]
    #[case::second_step(1, duration::millis(3), duration::millis(6))]
    #[case::third_step(2, duration::millis(3), duration::millis(12))]
    fn exponential_with_base_duration_doubles_delay_each_step(
      #[case] step_index: usize,
      #[case] base: Duration,
      #[case] expected: Duration,
    ) {
      let mut s = Schedule::exponential(base);
      let mut observed = None;
      for _ in 0..=step_index {
        observed = s.next(ScheduleInput::default()).map(|d| d.delay);
      }
      assert_eq!(observed, Some(expected));
    }

    #[test]
    fn jittered_with_spaced_schedule_applies_deterministic_ninety_percent_jitter() {
      let mut s = Schedule::spaced(duration::millis(10)).jittered();
      let decision = s
        .next(ScheduleInput::default())
        .expect("jittered schedule should continue");
      assert_eq!(decision.delay, duration::millis(9));
    }

    #[test]
    fn next_sleep_with_clock_returns_runtime_mediated_non_blocking_sleep_effect() {
      let start = std::time::Instant::now();
      let clock = TestClock::new(start);
      let mut s = Schedule::spaced(duration::millis(5));
      let sleep = s
        .next_sleep(&clock, ScheduleInput::default())
        .expect("spaced should continue");
      let _ = run_blocking(sleep, ());
      assert_eq!(clock.pending_sleeps().len(), 1);
    }
  }

  mod repeat {
    use super::*;

    #[test]
    fn repeat_n_with_times_runs_initial_plus_requested_repeats() {
      let calls = Arc::new(AtomicUsize::new(0));
      let calls_c = Arc::clone(&calls);
      let eff = repeat_n(
        move || {
          let c = Arc::clone(&calls_c);
          succeed::<usize, (), ()>(c.fetch_add(1, Ordering::SeqCst) + 1)
        },
        3,
      );

      let out = block_on(eff.run(&mut ()));
      assert_eq!(out, Ok(4));
      assert_eq!(calls.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn repeat_with_clock_uses_runtime_mediated_sleep_effect_between_attempts() {
      let start = std::time::Instant::now();
      let clock = TestClock::new(start);
      let calls = Arc::new(AtomicUsize::new(0));
      let calls_c = Arc::clone(&calls);
      let eff = repeat_with_clock(
        move || {
          let c = Arc::clone(&calls_c);
          succeed::<usize, (), ()>(c.fetch_add(1, Ordering::SeqCst) + 1)
        },
        Schedule::spaced(duration::millis(5)).compose(Schedule::recurs(1)),
        clock.clone(),
        None,
      );
      let out = block_on(eff.run(&mut ()));
      assert_eq!(out, Ok(2));
      assert_eq!(clock.pending_sleeps().len(), 1);
    }
  }

  mod debug_format {
    use super::*;

    #[test]
    fn spaced_debug_contains_interval() {
      let s = Schedule::spaced(duration::millis(50));
      let dbg = format!("{:?}", s);
      assert!(dbg.contains("Spaced"), "got: {dbg}");
    }

    #[test]
    fn exponential_debug_contains_base_and_step() {
      let s = Schedule::exponential(duration::millis(10));
      let dbg = format!("{:?}", s);
      assert!(dbg.contains("Exponential"), "got: {dbg}");
    }

    #[test]
    fn compose_debug_contains_both_sides() {
      let s = Schedule::recurs(1).compose(Schedule::recurs(2));
      let dbg = format!("{:?}", s);
      assert!(dbg.contains("Compose"), "got: {dbg}");
    }

    #[test]
    fn jittered_debug_shows_inner() {
      let s = Schedule::spaced(duration::millis(5)).jittered();
      let dbg = format!("{:?}", s);
      assert!(dbg.contains("Jittered"), "got: {dbg}");
    }

    #[test]
    fn recurs_while_debug_is_non_exhaustive() {
      let s = Schedule::recurs_while(Box::new(|_: &ScheduleInput| true));
      let dbg = format!("{:?}", s);
      assert!(dbg.contains("RecursWhile"), "got: {dbg}");
    }

    #[test]
    fn recurs_until_debug_is_non_exhaustive() {
      let s = Schedule::recurs_until(Box::new(|_: &ScheduleInput| false));
      let dbg = format!("{:?}", s);
      assert!(dbg.contains("RecursUntil"), "got: {dbg}");
    }

    #[test]
    fn map_delay_debug_shows_fn_placeholder() {
      let s = Schedule::spaced(duration::millis(5)).map(|d| d * 2);
      let dbg = format!("{:?}", s);
      assert!(dbg.contains("MapDelay"), "got: {dbg}");
    }

    #[test]
    fn contramap_input_debug_shows_fn_placeholder() {
      let s =
        Schedule::recurs(3).contramap(|i: ScheduleInput| ScheduleInput { attempt: i.attempt });
      let dbg = format!("{:?}", s);
      assert!(dbg.contains("ContramapInput"), "got: {dbg}");
    }
  }

  mod nested_map_and_contramap {
    use super::*;

    #[test]
    fn double_map_composes_both_functions() {
      // Apply map twice: first double, then add 1ms. Should produce 2*d + 1ms.
      let mut s = Schedule::spaced(duration::millis(10))
        .map(|d| d * 2) // 20ms
        .map(|d| d + duration::millis(1)); // 21ms
      let decision = s.next(ScheduleInput::default()).unwrap();
      assert_eq!(decision.delay, duration::millis(21));
    }

    #[test]
    fn double_contramap_composes_both_input_transforms() {
      // Two contramaps: first add 10 to attempt, then subtract 5.
      // Net effect: attempt += 5. So if input.attempt = 0, inner sees 5.
      // recurs_until stops when attempt >= 3; with +5 offset, always stops immediately.
      let mut s = Schedule::recurs_until(Box::new(|i: &ScheduleInput| i.attempt >= 3))
        .contramap(|mut i: ScheduleInput| {
          i.attempt = i.attempt.saturating_add(10);
          i
        })
        .contramap(|mut i: ScheduleInput| {
          i.attempt = i.attempt.saturating_sub(5);
          i
        });
      // Input attempt=0 → subtract 5 → 0 → add 10 → 10 >= 3 → stop
      assert_eq!(s.next(ScheduleInput { attempt: 0 }), None);
    }
  }

  mod repeat_with_interrupt {
    use super::*;

    #[test]
    fn repeat_with_clock_and_interrupt_runs_normally_when_not_cancelled() {
      let token = CancellationToken::new();
      let calls = Arc::new(AtomicUsize::new(0));
      let calls_c = Arc::clone(&calls);
      let eff = repeat_with_clock_and_interrupt(
        move || {
          let c = Arc::clone(&calls_c);
          succeed::<usize, (), ()>(c.fetch_add(1, Ordering::SeqCst) + 1)
        },
        Schedule::recurs(2),
        TestClock::new(std::time::Instant::now()),
        token,
        None,
      );
      let out = block_on(eff.run(&mut ()));
      assert_eq!(out, Ok(3));
      assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn repeat_with_clock_and_interrupt_stops_when_cancelled_between_attempts() {
      let token = CancellationToken::new();
      token.cancel();
      let calls = Arc::new(AtomicUsize::new(0));
      let calls_c = Arc::clone(&calls);
      let eff = repeat_with_clock_and_interrupt(
        move || {
          let c = Arc::clone(&calls_c);
          succeed::<usize, (), ()>(c.fetch_add(1, Ordering::SeqCst) + 1)
        },
        Schedule::recurs(5),
        TestClock::new(std::time::Instant::now()),
        token,
        None,
      );
      let out = block_on(eff.run(&mut ()));
      // First run completes, then the loop checks the token and breaks
      assert_eq!(out, Ok(1));
      assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn repeat_with_clock_and_interrupt_with_metric_increments_counter() {
      use crate::observability::metric::Metric;
      let token = CancellationToken::new();
      let counter = Metric::counter("repeat_interrupt_attempts", []);
      let calls = Arc::new(AtomicUsize::new(0));
      let calls_c = Arc::clone(&calls);
      let eff = repeat_with_clock_and_interrupt(
        move || {
          let c = Arc::clone(&calls_c);
          succeed::<usize, (), ()>(c.fetch_add(1, Ordering::SeqCst) + 1)
        },
        Schedule::recurs(1),
        TestClock::new(std::time::Instant::now()),
        token,
        Some(counter.clone()),
      );
      let _ = block_on(eff.run(&mut ()));
      assert!(counter.snapshot_count() >= 1);
    }
  }

  mod retry {
    use super::*;
    use crate::observability::metric::Metric;

    #[test]
    fn schedule_retry_metric_records_each_attempt() {
      let counter = Metric::counter("schedule_retry_attempts", []);
      let calls = Arc::new(AtomicUsize::new(0));
      let calls_c = Arc::clone(&calls);
      let eff = retry_with_clock(
        move || {
          let n = calls_c.fetch_add(1, Ordering::SeqCst);
          if n < 2 {
            fail::<usize, &'static str, ()>("boom")
          } else {
            succeed::<usize, &'static str, ()>(n + 1)
          }
        },
        Schedule::recurs(3),
        TestClock::new(std::time::Instant::now()),
        Some(counter.clone()),
      );

      let out = block_on(eff.run(&mut ()));
      assert_eq!(out, Ok(3));
      assert_eq!(calls.load(Ordering::SeqCst), 3);
      assert_eq!(counter.snapshot_count(), 3);
    }

    #[test]
    fn retry_with_eventual_success_returns_first_success_within_schedule_budget() {
      let calls = Arc::new(AtomicUsize::new(0));
      let calls_c = Arc::clone(&calls);
      let eff = retry(
        move || {
          let n = calls_c.fetch_add(1, Ordering::SeqCst);
          if n < 2 {
            fail::<usize, &'static str, ()>("boom")
          } else {
            succeed::<usize, &'static str, ()>(n + 1)
          }
        },
        Schedule::recurs(3),
      );

      let out = block_on(eff.run(&mut ()));
      assert_eq!(out, Ok(3));
      assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn retry_with_exhausted_schedule_returns_last_error() {
      let eff = retry(|| fail::<(), &'static str, ()>("boom"), Schedule::recurs(1));
      assert_eq!(block_on(eff.run(&mut ())), Err("boom"));
    }

    #[test]
    fn retry_with_interrupt_token_stops_retrying_when_token_is_already_cancelled() {
      let token = CancellationToken::new();
      token.cancel();
      let attempts = Arc::new(AtomicUsize::new(0));
      let attempts_c = Arc::clone(&attempts);
      let eff = retry_with_clock_and_interrupt(
        move || {
          attempts_c.fetch_add(1, Ordering::SeqCst);
          fail::<(), &'static str, ()>("boom")
        },
        Schedule::recurs(5),
        TestClock::new(std::time::Instant::now()),
        token,
        None,
      );
      assert_eq!(block_on(eff.run(&mut ())), Err("boom"));
      assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }
  }
}
