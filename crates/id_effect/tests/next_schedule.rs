//! `p-schedule` acceptance tests for `id_effect::next`: the immutable `Schedule<Out, In>`, its
//! combinators, and `Effect::{retry, retry_or_else, repeat, repeat_or_else}` on the virtual clock
//! of `TestExecutor`.

use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use id_effect::next::{
  Effect, Executor, FiberHandle, JitterSource, Schedule, TestExecutor, die, fail, succeed,
};
use id_effect::{Cause, Exit};

const SOURCE: &str = include_str!("../src/scheduling/schedule.rs");

fn ms(n: u64) -> Duration {
  Duration::from_millis(n)
}

fn counter() -> Arc<AtomicUsize> {
  Arc::new(AtomicUsize::new(0))
}

/// Polls `fut` once; it must be ready.
fn take_ready<F: Future>(fut: F) -> F::Output {
  let mut fut = pin!(fut);
  match fut.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
    Poll::Ready(v) => v,
    Poll::Pending => panic!("root fiber is not done"),
  }
}

/// Runs `eff` as a root fiber on a fresh `TestExecutor`, advancing the virtual clock only from
/// timer deadline to timer deadline; returns the exit and the virtual time it took.
fn run_virtual<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> (Exit<A, E>, Duration) {
  let exec = TestExecutor::new();
  let start = exec.now();
  let h = eff.fork_root(Arc::new(exec.clone()));
  exec.run_until_stalled();
  while !h.is_done() {
    let deadline = exec
      .next_timer_deadline()
      .expect("root fiber stalled with no timer pending");
    exec.advance(deadline.saturating_duration_since(exec.now()));
  }
  (take_ready(h.join_future()), exec.now() - start)
}

/// An effect factory that fails `fails` times (counting attempts in `n`), then succeeds with the
/// attempt index.
fn flaky(
  n: &Arc<AtomicUsize>,
  fails: usize,
) -> impl Fn() -> Effect<usize, String> + Send + Sync + use<> {
  let n = Arc::clone(n);
  move || {
    let i = n.fetch_add(1, Ordering::SeqCst);
    if i < fails {
      fail(format!("boom {i}"))
    } else {
      succeed(i)
    }
  }
}

/// Steps `s` `k` times at one instant with input `()`; returns `(output, delay)` per step.
fn steps<Out: 'static>(s: &Schedule<Out, ()>, k: usize) -> Vec<(Out, Option<Duration>)> {
  let now = Instant::now();
  let mut state = s.start();
  let mut got = Vec::with_capacity(k);
  for _ in 0..k {
    let (next, out, d) = s.step(now, &(), state).into_parts();
    state = next;
    got.push((out, d));
  }
  got
}

fn delays<Out: 'static>(s: &Schedule<Out, ()>, k: usize) -> Vec<Option<Duration>> {
  steps(s, k).into_iter().map(|(_, d)| d).collect()
}

fn outs<Out: 'static>(s: &Schedule<Out, ()>, k: usize) -> Vec<Out> {
  steps(s, k).into_iter().map(|(o, _)| o).collect()
}

// ── AC: immutable schedule, no free functions ─────────────────────────────────────────────

#[test]
fn schedule_has_no_mutable_next_and_no_free_retry_or_repeat() {
  assert!(!SOURCE.contains("fn next(&mut self"));
  for free in [
    "\npub fn retry",
    "\npub fn repeat",
    "\npub fn retry_or_else",
    "\npub fn repeat_or_else",
  ] {
    assert!(!SOURCE.contains(free), "free fn found: {free:?}");
  }
}

#[test]
fn one_schedule_value_drives_independent_runs() {
  let s = Schedule::<u64, ()>::recurs(2);
  let mut a = s.start();
  for _ in 0..3 {
    a = s.step(Instant::now(), &(), a).into_parts().0;
  }
  // `a` is exhausted; a fresh run of the same value is not.
  assert!(s.step(Instant::now(), &(), a).is_done());
  let fresh = s.step(Instant::now(), &(), s.start());
  assert_eq!(*fresh.out(), 0);
  assert_eq!(fresh.delay(), Some(Duration::ZERO));
}

// ── AC: TestClock, exponential(10ms) with 3 retries takes exactly 70 ms ───────────────────

#[test]
fn exponential_10ms_with_3_retries_takes_exactly_70ms_of_virtual_time() {
  let n = counter();
  let eff = Effect::retry(
    flaky(&n, usize::MAX),
    Schedule::exponential(ms(10)).intersect(Schedule::recurs(3)),
  );
  let (exit, took) = run_virtual(eff);
  assert_eq!(took, ms(70));
  assert_eq!(n.load(Ordering::SeqCst), 4, "1 attempt + 3 retries");
  assert!(matches!(exit, Exit::Failure(Cause::Fail(ref e)) if e == "boom 3"));
}

#[test]
fn exponential_retry_is_not_done_one_nanosecond_before_70ms() {
  let n = counter();
  let eff: Effect<usize, String> = Effect::retry(
    flaky(&n, usize::MAX),
    Schedule::exponential(ms(10)).intersect(Schedule::recurs(3)),
  );
  let exec = TestExecutor::new();
  let h: FiberHandle<usize, String> = eff.fork_root(Arc::new(exec.clone()));
  exec.run_until_stalled();
  for step in [ms(10), ms(20), ms(40) - Duration::from_nanos(1)] {
    exec.advance(step);
  }
  assert!(!h.is_done());
  assert_eq!(n.load(Ordering::SeqCst), 3);
  exec.advance(Duration::from_nanos(1));
  assert!(h.is_done());
  assert_eq!(n.load(Ordering::SeqCst), 4);
}

// ── retry / retry_or_else ─────────────────────────────────────────────────────────────────

#[test]
fn retry_succeeds_once_the_effect_does() {
  let n = counter();
  let (exit, took) = run_virtual(Effect::retry(flaky(&n, 2), Schedule::spaced(ms(5))));
  assert!(matches!(exit, Exit::Success(2)));
  assert_eq!(took, ms(10));
  assert_eq!(n.load(Ordering::SeqCst), 3);
}

#[test]
fn retry_does_not_call_the_factory_before_it_runs() {
  let n = counter();
  let eff = Effect::retry(flaky(&n, 0), Schedule::recurs(1));
  assert_eq!(n.load(Ordering::SeqCst), 0);
  assert!(matches!(eff.block_on(), Exit::Success(0)));
}

#[test]
fn retry_never_retries_a_defect() {
  let n = counter();
  let m = Arc::clone(&n);
  let eff: Effect<(), String> = Effect::retry(
    move || {
      m.fetch_add(1, Ordering::SeqCst);
      die("bug")
    },
    Schedule::recurs(5),
  );
  let (exit, _) = run_virtual(eff);
  assert!(matches!(exit, Exit::Failure(Cause::Die(_))));
  assert_eq!(n.load(Ordering::SeqCst), 1);
}

#[test]
fn retry_or_else_recovers_with_last_error_and_output() {
  let n = counter();
  let eff: Effect<String, std::convert::Infallible> = Effect::retry_or_else(
    move || flaky(&n, usize::MAX)().map(|i| i.to_string()),
    Schedule::recurs(2),
    |e, out| succeed(format!("{e} after {out}")),
  );
  let (exit, _) = run_virtual(eff);
  assert!(matches!(exit, Exit::Success(ref s) if s == "boom 2 after 2"));
}

#[test]
fn retry_while_input_stops_on_a_fatal_error() {
  let n = counter();
  let eff = Effect::retry(
    flaky(&n, usize::MAX),
    Schedule::forever().while_input(|e: &String| e != "boom 1"),
  );
  let (exit, _) = run_virtual(eff);
  assert!(matches!(exit, Exit::Failure(Cause::Fail(ref e)) if e == "boom 1"));
  assert_eq!(n.load(Ordering::SeqCst), 2);
}

#[test]
fn two_runs_share_one_schedule_value() {
  let s: Schedule<(Duration, u64), String> =
    Schedule::exponential(ms(10)).intersect(Schedule::recurs(3));
  let (a, b) = (counter(), counter());
  let eff = Effect::retry(flaky(&a, usize::MAX), s.clone())
    .exit::<String>()
    .zip(Effect::retry(flaky(&b, usize::MAX), s).exit());
  let (exit, took) = run_virtual(eff);
  assert!(matches!(exit, Exit::Success(_)));
  assert_eq!(took, ms(140), "sequential runs, each 70 ms");
  assert_eq!(a.load(Ordering::SeqCst), 4);
  assert_eq!(b.load(Ordering::SeqCst), 4);
}

// ── repeat / repeat_or_else ───────────────────────────────────────────────────────────────

#[test]
fn repeat_runs_one_plus_n_times_and_returns_the_last_output() {
  let n = counter();
  let (exit, took) = run_virtual(Effect::repeat(
    flaky(&n, 0),
    Schedule::spaced(ms(100)).intersect(Schedule::recurs(3)),
  ));
  assert!(matches!(exit, Exit::Success((3, 3))));
  assert_eq!(took, ms(300));
  assert_eq!(n.load(Ordering::SeqCst), 4);
}

#[test]
fn repeat_stops_at_the_first_failure() {
  let n = counter();
  let m = Arc::clone(&n);
  let eff: Effect<u64, String> = Effect::repeat(
    move || {
      let i = m.fetch_add(1, Ordering::SeqCst);
      if i == 2 {
        fail(format!("stop {i}"))
      } else {
        succeed(i)
      }
    },
    Schedule::recurs(10),
  );
  let (exit, _) = run_virtual(eff);
  assert!(matches!(exit, Exit::Failure(Cause::Fail(ref e)) if e == "stop 2"));
  assert_eq!(n.load(Ordering::SeqCst), 3);
}

#[test]
fn repeat_or_else_sees_the_last_output_so_far() {
  let n = counter();
  let m = Arc::clone(&n);
  let eff: Effect<u64, String> = Effect::repeat_or_else(
    move || {
      let i = m.fetch_add(1, Ordering::SeqCst);
      if i == 2 { fail("stop") } else { succeed(i) }
    },
    Schedule::recurs(10),
    |_e, last| succeed(last.map_or(99, |o| o + 100)),
  );
  let (exit, _) = run_virtual(eff);
  assert!(matches!(exit, Exit::Success(101)));

  let first_fails: Effect<u64, String> = Effect::repeat_or_else(
    || fail("at once"),
    Schedule::<u64, ()>::recurs(10),
    |_e, last| succeed(last.map_or(99, |o| o + 100)),
  );
  assert!(matches!(run_virtual(first_fails).0, Exit::Success(99)));
}

#[test]
fn repeat_until_input_via_while_input() {
  let n = counter();
  let (exit, _) = run_virtual(Effect::repeat(
    flaky(&n, 0),
    Schedule::forever().while_input(|i: &usize| *i < 4),
  ));
  assert!(matches!(exit, Exit::Success(4)));
  assert_eq!(n.load(Ordering::SeqCst), 5);
}

// ── combinators ───────────────────────────────────────────────────────────────────────────

#[test]
fn recurs_allows_exactly_n_recurrences() {
  let s = Schedule::<u64, ()>::recurs(3);
  let z = Some(Duration::ZERO);
  assert_eq!(delays(&s, 5), vec![z, z, z, None, None]);
  assert_eq!(outs(&s, 4), vec![0, 1, 2, 3]);
  assert_eq!(delays(&Schedule::<u64, ()>::recurs(0), 1), vec![None]);
}

#[test]
fn spaced_and_forever_never_stop() {
  assert_eq!(
    delays(&Schedule::<u64, ()>::spaced(ms(7)), 3),
    vec![Some(ms(7)); 3]
  );
  assert_eq!(outs(&Schedule::<u64, ()>::forever(), 3), vec![0, 1, 2]);
}

#[test]
fn exponential_doubles_from_base() {
  let s = Schedule::<Duration, ()>::exponential(ms(10));
  assert_eq!(
    delays(&s, 4),
    vec![Some(ms(10)), Some(ms(20)), Some(ms(40)), Some(ms(80))]
  );
  assert_eq!(outs(&s, 2), vec![ms(10), ms(20)]);
}

#[test]
fn fibonacci_delays() {
  let s = Schedule::<Duration, ()>::fibonacci(ms(10));
  let want: Vec<_> = [10, 10, 20, 30, 50, 80]
    .into_iter()
    .map(|n| Some(ms(n)))
    .collect();
  assert_eq!(delays(&s, 6), want);
}

#[test]
fn union_takes_the_shorter_delay_and_continues_while_either_does() {
  let s = Schedule::<u64, ()>::spaced(ms(50))
    .intersect(Schedule::recurs(1))
    .union(Schedule::<Duration, ()>::exponential(ms(10)).intersect(Schedule::recurs(3)));
  assert_eq!(
    delays(&s, 5),
    vec![Some(ms(10)), Some(ms(20)), Some(ms(40)), None, None]
  );
  let only_left = Schedule::<u64, ()>::spaced(ms(5)).union(Schedule::<u64, ()>::recurs(0));
  assert_eq!(delays(&only_left, 2), vec![Some(ms(5)); 2]);
}

#[test]
fn intersect_takes_the_longer_delay_and_stops_when_either_does() {
  let s = Schedule::<u64, ()>::spaced(ms(15)).intersect(Schedule::exponential(ms(10)));
  assert_eq!(
    delays(&s, 3),
    vec![Some(ms(15)), Some(ms(20)), Some(ms(40))]
  );
  assert_eq!(outs(&s, 2), vec![(0, ms(10)), (1, ms(20))]);
}

#[test]
fn and_then_runs_the_second_after_the_first() {
  let first = Schedule::<u64, ()>::spaced(ms(1))
    .intersect(Schedule::recurs(2))
    .map(|(_, n)| n);
  let second = Schedule::<u64, ()>::spaced(ms(9))
    .intersect(Schedule::recurs(1))
    .map(|(n, _)| n + 100);
  let s = first.and_then(second);
  assert_eq!(
    delays(&s, 5),
    vec![Some(ms(1)), Some(ms(1)), Some(ms(9)), None, None]
  );
  assert_eq!(outs(&s, 4), vec![0, 1, 100, 101]);
}

#[test]
fn up_to_stops_once_the_budget_has_elapsed() {
  let s = Schedule::<u64, ()>::spaced(ms(10)).up_to(ms(25));
  let t0 = Instant::now();
  let mut state = s.start();
  let mut got = Vec::new();
  for k in 0..5u64 {
    let (next, _, d) = s.step(t0 + ms(10 * k), &(), state).into_parts();
    state = next;
    got.push(d);
  }
  assert_eq!(
    got,
    vec![Some(ms(10)), Some(ms(10)), Some(ms(10)), None, None]
  );
}

#[test]
fn up_to_bounds_a_retry_on_virtual_time() {
  let n = counter();
  let (exit, took) = run_virtual(Effect::retry(
    flaky(&n, usize::MAX),
    Schedule::spaced(ms(10)).up_to(ms(35)),
  ));
  assert!(matches!(exit, Exit::Failure(Cause::Fail(_))));
  assert_eq!(took, ms(40));
  assert_eq!(n.load(Ordering::SeqCst), 5);
}

#[test]
fn jittered_with_seed_is_reproducible_and_in_range() {
  let s = Schedule::<u64, ()>::spaced(ms(100)).jittered_with(0.8, 1.2, JitterSource::seeded(7));
  let a = delays(&s, 50);
  assert_eq!(a, delays(&s, 50), "same seed, same sequence");
  assert!(a.iter().all(|d| {
    let d = d.expect("spaced never stops");
    (ms(80)..=ms(120)).contains(&d)
  }));
  assert!(
    a.windows(2).any(|w| w[0] != w[1]),
    "jitter varies the delay"
  );
  let other = Schedule::<u64, ()>::spaced(ms(100)).jittered_with(0.8, 1.2, JitterSource::seeded(8));
  assert_ne!(a, delays(&other, 50));
}

#[test]
fn jittered_default_stays_in_range_and_keeps_done() {
  let s = Schedule::<u64, ()>::spaced(ms(100))
    .intersect(Schedule::recurs(20))
    .jittered();
  let d = delays(&s, 22);
  assert!(
    d[..20]
      .iter()
      .all(|d| (ms(80)..=ms(120)).contains(&d.expect("continues")))
  );
  assert_eq!(&d[20..], &[None, None]);
  let custom =
    Schedule::<u64, ()>::spaced(ms(10)).jittered_with(-1.0, -0.5, JitterSource::from_fn(|| 42));
  assert_eq!(delays(&custom, 1), vec![Some(Duration::ZERO)]);
}

#[test]
fn map_transforms_the_output_only() {
  let s = Schedule::<u64, ()>::spaced(ms(3)).map(|n| n * 10);
  assert_eq!(outs(&s, 3), vec![0, 10, 20]);
  assert_eq!(delays(&s, 1), vec![Some(ms(3))]);
}

#[test]
fn from_fn_sees_the_input() {
  let s: Schedule<usize, &'static str> = Schedule::from_fn(
    || 0usize,
    |_, input: &&'static str, n| (n + 1, input.len(), (n < 1).then_some(ms(1))),
  );
  let step = s.step(Instant::now(), &"abc", s.start());
  assert_eq!(*step.out(), 3);
  assert!(!step.is_done());
  let (state, _, _) = step.into_parts();
  assert!(s.step(Instant::now(), &"x", state).is_done());
}
