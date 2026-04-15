//! Runtime-mediated clock services.

use crate::collections::sorted_map::{self, EffectSortedMap};
use crate::runtime::{Never, Runtime};
use crate::scheduling::datetime::UtcDateTime;
use crate::{Effect, succeed};
use jiff::Timestamp;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Clock service contract used by schedule/test runtime integrations.
pub trait Clock {
  /// Current time from this clock (typically monotonic via the runtime).
  fn now(&self) -> Instant;
  /// Suspend until `duration` has elapsed.
  fn sleep(&self, duration: Duration) -> Effect<(), Never, ()>;
  /// Suspend until `deadline`; succeeds immediately if `deadline` is not in the future.
  fn sleep_until(&self, deadline: Instant) -> Effect<(), Never, ()>;
}

/// Production clock backed by the active runtime.
pub struct LiveClock<R: Runtime> {
  runtime: Arc<R>,
}

impl<R: Runtime> Clone for LiveClock<R> {
  fn clone(&self) -> Self {
    Self {
      runtime: Arc::clone(&self.runtime),
    }
  }
}

impl<R: Runtime> LiveClock<R> {
  /// Wrap `runtime` as a [`LiveClock`].
  #[inline]
  pub fn new(runtime: R) -> Self {
    Self {
      runtime: Arc::new(runtime),
    }
  }

  /// Same as [`LiveClock::new`] but shares an existing [`Arc`] handle.
  #[inline]
  pub fn from_arc(runtime: Arc<R>) -> Self {
    Self { runtime }
  }

  /// Wall-clock UTC instant from the system clock ([`Timestamp::now`]).
  ///
  /// This is independent of [`Clock::now`]: the runtime’s [`Instant`] is for monotonic scheduling;
  /// this value is calendar time suitable for log timestamps and human-facing output.
  #[inline]
  pub fn now_utc(&self) -> UtcDateTime {
    UtcDateTime(Timestamp::now())
  }
}

impl<R: Runtime> Clock for LiveClock<R> {
  #[inline]
  fn now(&self) -> Instant {
    self.runtime.now()
  }

  #[inline]
  fn sleep(&self, duration: Duration) -> Effect<(), Never, ()> {
    self.runtime.sleep(duration)
  }

  #[inline]
  fn sleep_until(&self, deadline: Instant) -> Effect<(), Never, ()> {
    let now = self.runtime.now();
    if deadline <= now {
      return succeed::<(), Never, ()>(());
    }
    self.runtime.sleep(deadline.duration_since(now))
  }
}

/// Deterministic clock for tests with manual virtual-time controls.
#[derive(Clone)]
pub struct TestClock {
  now: Arc<Mutex<Instant>>,
  /// Deadline → number of pending sleeps registered for that instant (sorted map keys iterate in order).
  pending: Arc<Mutex<EffectSortedMap<Instant, usize>>>,
}

impl TestClock {
  /// Virtual clock starting at `now` with no registered sleeps.
  #[inline]
  pub fn new(now: Instant) -> Self {
    Self {
      now: Arc::new(Mutex::new(now)),
      pending: Arc::new(Mutex::new(sorted_map::empty())),
    }
  }

  /// Set the current instant and drop pending sleeps at or before `now`.
  #[inline]
  pub fn set_time(&self, now: Instant) {
    *self.now.lock().expect("test clock now mutex poisoned") = now;
    self.retain_pending_after(now);
  }

  /// Advance virtual time by `by` and drop sleeps that have elapsed.
  #[inline]
  pub fn advance(&self, by: Duration) {
    let mut guard = self.now.lock().expect("test clock now mutex poisoned");
    *guard += by;
    let now = *guard;
    drop(guard);
    self.retain_pending_after(now);
  }

  /// Registered sleep deadlines (sorted; duplicates mean concurrent sleeps to the same instant).
  #[inline]
  pub fn pending_sleeps(&self) -> Vec<Instant> {
    let map = self
      .pending
      .lock()
      .expect("test clock pending mutex poisoned");
    expand_pending_deadlines(&map)
  }

  fn retain_pending_after(&self, now: Instant) {
    let mut map = self
      .pending
      .lock()
      .expect("test clock pending mutex poisoned");
    *map = sorted_map::filter(map.clone(), |deadline, _| *deadline > now);
  }
}

fn add_pending_deadline(map: &mut EffectSortedMap<Instant, usize>, deadline: Instant) {
  let n = sorted_map::get(map, &deadline).unwrap_or(0);
  *map = sorted_map::set(map.clone(), deadline, n + 1);
}

fn expand_pending_deadlines(map: &EffectSortedMap<Instant, usize>) -> Vec<Instant> {
  let mut v = Vec::new();
  for (deadline, count) in map.iter() {
    v.extend(std::iter::repeat_n(*deadline, *count));
  }
  v
}

impl Clock for TestClock {
  #[inline]
  fn now(&self) -> Instant {
    *self.now.lock().expect("test clock now mutex poisoned")
  }

  #[inline]
  fn sleep(&self, duration: Duration) -> Effect<(), Never, ()> {
    let clock = self.clone();
    Effect::new(move |_env| {
      if duration.is_zero() {
        return Ok(());
      }
      let now = *clock.now.lock().expect("test clock now mutex poisoned");
      let deadline = now + duration;
      let mut pending = clock
        .pending
        .lock()
        .expect("test clock pending mutex poisoned");
      add_pending_deadline(&mut pending, deadline);
      Ok(())
    })
  }

  #[inline]
  fn sleep_until(&self, deadline: Instant) -> Effect<(), Never, ()> {
    let clock = self.clone();
    Effect::new(move |_env| {
      let now = *clock.now.lock().expect("test clock now mutex poisoned");
      if deadline <= now {
        return Ok(());
      }
      let mut pending = clock
        .pending
        .lock()
        .expect("test clock pending mutex poisoned");
      add_pending_deadline(&mut pending, deadline);
      Ok(())
    })
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::runtime::FiberHandle;
  use rstest::rstest;

  #[derive(Clone)]
  struct MockRuntime {
    now: Instant,
    sleeps: Arc<Mutex<Vec<Duration>>>,
  }

  impl MockRuntime {
    fn new(now: Instant) -> Self {
      Self {
        now,
        sleeps: Arc::new(Mutex::new(Vec::new())),
      }
    }
  }

  impl Runtime for MockRuntime {
    fn spawn_with<A, E, Env, F>(&self, f: F) -> FiberHandle<A, E>
    where
      A: Clone + Send + Sync + 'static,
      E: Clone + Send + Sync + 'static,
      Env: 'static,
      F: FnOnce() -> (Effect<A, E, Env>, Env) + Send + 'static,
    {
      crate::runtime::ThreadSleepRuntime.spawn_with(f)
    }

    fn sleep(&self, duration: Duration) -> Effect<(), Never, ()> {
      let sleeps = Arc::clone(&self.sleeps);
      Effect::new(move |_env| {
        sleeps
          .lock()
          .expect("sleep log mutex poisoned")
          .push(duration);
        Ok(())
      })
    }

    fn now(&self) -> Instant {
      self.now
    }

    fn yield_now(&self) -> Effect<(), Never, ()> {
      succeed(())
    }
  }

  fn run(effect: Effect<(), Never, ()>) {
    let _ = crate::runtime::run_blocking(effect, ());
  }

  mod live_clock {
    use super::*;
    use crate::scheduling::datetime::UtcDateTime;

    #[test]
    fn now_when_called_proxies_runtime_now() {
      let now = Instant::now();
      let clock = LiveClock::new(MockRuntime::new(now));
      assert_eq!(clock.now(), now);
    }

    #[test]
    fn from_arc_when_constructed_proxies_runtime_now() {
      let now = Instant::now();
      let runtime = Arc::new(MockRuntime::new(now));
      let clock = LiveClock::from_arc(runtime);
      assert_eq!(clock.now(), now);
    }

    #[test]
    fn live_clock_now_utc_is_after_epoch() {
      let now = Instant::now();
      let clock = LiveClock::new(MockRuntime::new(now));
      let utc = clock.now_utc();
      assert!(
        utc.greater_than(&UtcDateTime::unsafe_make(0)),
        "expected wall clock after Unix epoch, got {}",
        utc.format_iso()
      );
    }

    #[test]
    fn sleep_with_duration_records_requested_duration() {
      let now = Instant::now();
      let runtime = MockRuntime::new(now);
      let sleeps_ref = Arc::clone(&runtime.sleeps);
      let clock = LiveClock::new(runtime);
      run(clock.sleep(Duration::from_millis(7)));
      assert_eq!(
        sleeps_ref.lock().expect("sleep log mutex poisoned").clone(),
        vec![Duration::from_millis(7)]
      );
    }

    #[test]
    fn sleep_until_with_future_deadline_sleeps_only_remaining_duration() {
      let now = Instant::now();
      let runtime = MockRuntime::new(now);
      let sleeps_ref = Arc::clone(&runtime.sleeps);
      let clock = LiveClock::new(runtime);
      let deadline = now + Duration::from_millis(25);

      run(clock.sleep_until(deadline));

      assert_eq!(
        sleeps_ref.lock().expect("sleep log mutex poisoned").clone(),
        vec![Duration::from_millis(25)]
      );
    }

    #[rstest]
    #[case::past(Duration::from_millis(1))]
    #[case::exact_now(Duration::ZERO)]
    fn sleep_until_with_non_future_deadline_records_no_sleep(#[case] delta_from_now: Duration) {
      let now = Instant::now();
      let runtime = MockRuntime::new(now);
      let sleeps_ref = Arc::clone(&runtime.sleeps);
      let clock = LiveClock::new(runtime);
      let deadline = now.checked_sub(delta_from_now).unwrap_or(now);

      run(clock.sleep_until(deadline));

      assert!(
        sleeps_ref
          .lock()
          .expect("sleep log mutex poisoned")
          .is_empty()
      );
    }
  }

  mod test_clock {
    use super::*;

    #[test]
    fn now_when_new_returns_initial_time() {
      let start = Instant::now();
      let clock = TestClock::new(start);
      assert_eq!(clock.now(), start);
    }

    #[test]
    fn sleep_with_zero_duration_does_not_add_pending_deadline() {
      let clock = TestClock::new(Instant::now());
      run(clock.sleep(Duration::ZERO));
      assert!(clock.pending_sleeps().is_empty());
    }

    #[test]
    fn sleep_with_multiple_durations_tracks_sorted_pending_deadlines() {
      let start = Instant::now();
      let clock = TestClock::new(start);
      run(clock.sleep(Duration::from_millis(15)));
      run(clock.sleep(Duration::from_millis(5)));
      run(clock.sleep(Duration::from_millis(25)));

      let pending = clock.pending_sleeps();
      assert_eq!(
        pending,
        vec![
          start + Duration::from_millis(5),
          start + Duration::from_millis(15),
          start + Duration::from_millis(25),
        ]
      );
    }

    #[test]
    fn sleep_until_with_future_deadline_adds_pending_deadline() {
      let start = Instant::now();
      let clock = TestClock::new(start);
      let deadline = start + Duration::from_millis(20);

      run(clock.sleep_until(deadline));

      assert_eq!(clock.pending_sleeps(), vec![deadline]);
    }

    #[rstest]
    #[case::past(Duration::from_millis(1))]
    #[case::exact_now(Duration::ZERO)]
    fn sleep_until_with_non_future_deadline_adds_no_pending_deadline(
      #[case] delta_from_now: Duration,
    ) {
      let start = Instant::now();
      let clock = TestClock::new(start);
      let deadline = start.checked_sub(delta_from_now).unwrap_or(start);

      run(clock.sleep_until(deadline));

      assert!(clock.pending_sleeps().is_empty());
    }

    #[test]
    fn advance_with_elapsed_duration_clears_completed_pending_sleeps() {
      let start = Instant::now();
      let clock = TestClock::new(start);
      run(clock.sleep(Duration::from_millis(5)));
      run(clock.sleep(Duration::from_millis(15)));
      assert_eq!(clock.pending_sleeps().len(), 2);

      clock.advance(Duration::from_millis(6));
      assert_eq!(
        clock.pending_sleeps(),
        vec![start + Duration::from_millis(15)]
      );

      clock.advance(Duration::from_millis(10));
      assert!(clock.pending_sleeps().is_empty());
    }

    #[test]
    fn set_time_with_future_instant_clears_completed_pending_sleeps() {
      let start = Instant::now();
      let clock = TestClock::new(start);
      run(clock.sleep(Duration::from_millis(20)));
      assert_eq!(clock.pending_sleeps().len(), 1);

      clock.set_time(start + Duration::from_millis(25));

      assert!(clock.pending_sleeps().is_empty());
    }

    #[test]
    fn test_clock_multiple_sleeps_same_instant_all_wake() {
      let start = Instant::now();
      let clock = TestClock::new(start);
      let d = Duration::from_millis(10);
      run(clock.sleep(d));
      run(clock.sleep(d));
      run(clock.sleep(d));
      let t = start + d;
      assert_eq!(clock.pending_sleeps(), vec![t, t, t]);
      clock.advance(d);
      assert!(clock.pending_sleeps().is_empty());
    }

    #[test]
    fn test_clock_advance_wakes_expired_sleeps_in_order() {
      let start = Instant::now();
      let clock = TestClock::new(start);
      run(clock.sleep(Duration::from_millis(5)));
      run(clock.sleep(Duration::from_millis(15)));
      run(clock.sleep(Duration::from_millis(10)));
      assert_eq!(
        clock.pending_sleeps(),
        vec![
          start + Duration::from_millis(5),
          start + Duration::from_millis(10),
          start + Duration::from_millis(15),
        ]
      );
      clock.advance(Duration::from_millis(5));
      assert_eq!(
        clock.pending_sleeps(),
        vec![
          start + Duration::from_millis(10),
          start + Duration::from_millis(15),
        ]
      );
      clock.advance(Duration::from_millis(5));
      assert_eq!(
        clock.pending_sleeps(),
        vec![start + Duration::from_millis(15)],
      );
      clock.advance(Duration::from_millis(10));
      assert!(clock.pending_sleeps().is_empty());
    }
  }
}
