//! `c-race-timeout` acceptance tests for `id_effect`: `race`, `race_all`, `race_first`,
//! `timeout` and `timeout_fail`.
//!
//! Behavioural tests run on a multi-thread `TokioExecutor` **and** on the deterministic
//! `TestExecutor` through [`on_both`]; the virtual-clock tests drive a `TestExecutor` by hand.

use std::future::Future;
use std::pin::pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use id_effect::Never;
use id_effect::{Cause, Exit};
use id_effect::{
  Effect, Executor, TestExecutor, TokioExecutor, die, fail, race_all, sleep, succeed, suspend,
  sync, yield_fiber,
};

const HOUR: Duration = Duration::from_secs(3600);
const MS: Duration = Duration::from_millis(1);
const TEN_S: Duration = Duration::from_secs(10);

// ── drivers ───────────────────────────────────────────────────────────────────────────────

fn tokio_rt() -> &'static tokio::runtime::Runtime {
  static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
  RT.get_or_init(|| {
    tokio::runtime::Builder::new_multi_thread()
      .worker_threads(4)
      .enable_time()
      .build()
      .expect("tokio runtime")
  })
}

/// Polls `fut` once; it must be ready (the test executor has already run everything).
fn take_ready<F: Future>(fut: F) -> F::Output {
  let mut fut = pin!(fut);
  match fut.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
    Poll::Ready(v) => v,
    Poll::Pending => panic!("root fiber is not done"),
  }
}

#[derive(Clone, Copy, Debug)]
enum Driver {
  Tokio,
  Test,
}

impl Driver {
  /// Runs `eff` as a root fiber and returns its exit.
  fn run<A: Send + 'static, E: Send + 'static>(self, eff: Effect<A, E>) -> Exit<A, E> {
    match self {
      Driver::Tokio => {
        let rt = tokio_rt();
        let h = eff.fork_root(Arc::new(TokioExecutor::new(rt.handle().clone())));
        rt.block_on(async {
          tokio::time::timeout(Duration::from_secs(30), h.join_future())
            .await
            .expect("root fiber timed out on tokio")
        })
      }
      Driver::Test => {
        let exec = TestExecutor::new();
        let h = eff.fork_root(Arc::new(exec.clone()));
        exec.run_until_stalled();
        while !h.is_done() {
          let deadline = exec
            .next_timer_deadline()
            .expect("root fiber stalled with no timer pending");
          exec.advance(deadline.saturating_duration_since(exec.now()));
        }
        take_ready(h.join_future())
      }
    }
  }
}

/// Runs a test body once per executor.
fn on_both(t: impl Fn(Driver)) {
  t(Driver::Tokio);
  t(Driver::Test);
}

// ── helpers ───────────────────────────────────────────────────────────────────────────────

fn flag() -> Arc<AtomicBool> {
  Arc::new(AtomicBool::new(false))
}

fn set(f: &Arc<AtomicBool>) -> Effect<(), Never> {
  let f = Arc::clone(f);
  sync(move || f.store(true, Ordering::SeqCst))
}

fn is_set(f: &Arc<AtomicBool>) -> bool {
  f.load(Ordering::SeqCst)
}

/// Succeeds with `v` after `d`.
fn after<E: Send + 'static>(d: Duration, v: u8) -> Effect<u8, E> {
  sleep(d).map(move |()| v)
}

/// Fails with `e` after `d`.
fn fail_after(d: Duration, e: u8) -> Effect<u8, u8> {
  sleep(d).flat_map(move |()| fail(e))
}

fn counter() -> Arc<AtomicUsize> {
  Arc::new(AtomicUsize::new(0))
}

/// A racer that never wins: inside its finalized region it bumps `go`, then sleeps for an hour;
/// `fin` is its finalizer.
fn loser<E: Send + 'static>(go: &Arc<AtomicUsize>, fin: Effect<(), Never>) -> Effect<u8, E> {
  let go = Arc::clone(go);
  sync(move || {
    go.fetch_add(1, Ordering::SeqCst);
  })
  .then(sleep(HOUR))
  .map(|()| 0)
  .ensuring(fin)
}

/// Yields until `go` has reached `n` (every loser is inside its finalized region), then `then`.
fn once_started<A: Send + 'static, E: Send + 'static>(
  go: &Arc<AtomicUsize>,
  n: usize,
  then: Effect<A, E>,
) -> Effect<A, E> {
  fn wait<E: Send + 'static>(go: Arc<AtomicUsize>, n: usize) -> Effect<(), E> {
    suspend(move || {
      if go.load(Ordering::SeqCst) >= n {
        succeed(())
      } else {
        yield_fiber().flat_map(move |()| wait(go, n))
      }
    })
  }
  wait(Arc::clone(go), n).then(then)
}

/// Pairs `eff`'s value with the state of `fin` read right after `eff` returned.
fn then_read<A: Send + 'static, E: Send + 'static>(
  eff: Effect<A, E>,
  fin: &Arc<AtomicBool>,
) -> Effect<(A, bool), E> {
  let fin = Arc::clone(fin);
  eff.flat_map(move |a| sync(move || (a, fin.load(Ordering::SeqCst))))
}

fn success<A: std::fmt::Debug, E: std::fmt::Debug>(d: Driver, exit: Exit<A, E>) -> A {
  match exit {
    Exit::Success(a) => a,
    Exit::Failure(c) => panic!("{d:?}: expected success, got {c:?}"),
  }
}

fn failure<A: std::fmt::Debug, E: std::fmt::Debug>(d: Driver, exit: Exit<A, E>) -> Cause<E> {
  match exit {
    Exit::Failure(c) => c,
    Exit::Success(a) => panic!("{d:?}: expected failure, got {a:?}"),
  }
}

// ── AC1: race / race_all return the first success ─────────────────────────────────────────

#[test]
fn race_returns_the_first_success() {
  on_both(|d| {
    let eff: Effect<u8> = after(HOUR, 1).race(after(10 * MS, 2));
    assert_eq!(success(d, d.run(eff)), 2, "{d:?}");

    let eff: Effect<u8> = after(10 * MS, 1).race(after(HOUR, 2));
    assert_eq!(success(d, d.run(eff)), 1, "{d:?}");
  });
}

#[test]
fn race_a_failure_does_not_end_the_race() {
  on_both(|d| {
    let eff: Effect<u8, u8> = fail(7).race(after(20 * MS, 3));
    assert_eq!(success(d, d.run(eff)), 3, "{d:?}");
  });
}

#[test]
fn race_all_returns_the_first_success() {
  on_both(|d| {
    let eff: Effect<u8, u8> = race_all([
      fail(1),
      after(HOUR, 9),
      fail_after(5 * MS, 2),
      after(30 * MS, 7),
      after(60 * MS, 8),
    ]);
    assert_eq!(success(d, d.run(eff)), 7, "{d:?}");
  });
}

#[test]
fn race_all_of_no_effects_dies() {
  on_both(|d| {
    let eff: Effect<u8, u8> = race_all(Vec::new());
    match failure(d, d.run(eff)) {
      Cause::Die(defect) => assert_eq!(defect.message(), "id_effect: race_all of no effects"),
      other => panic!("{d:?}: expected Die, got {other:?}"),
    }
  });
}

// ── AC2: the loser's finalizer has run before the result is returned ──────────────────────

#[test]
fn race_loser_finalizer_has_run_before_the_result() {
  on_both(|d| {
    let (go, fin) = (counter(), flag());
    let racer: Effect<u8> = loser(&go, set(&fin));
    let eff = then_read(racer.race(once_started(&go, 1, succeed(1))), &fin);
    assert_eq!(success(d, d.run(eff)), (1, true), "{d:?}");
  });
}

#[test]
fn race_all_losers_finalizers_have_run_before_the_result() {
  on_both(|d| {
    let go = counter();
    let fins: Vec<Arc<AtomicBool>> = (0..3).map(|_| flag()).collect();
    let mut racers: Vec<Effect<u8>> = fins.iter().map(|f| loser(&go, set(f))).collect();
    racers.insert(1, once_started(&go, 3, succeed(5)));
    let read = fins.clone();
    let eff = race_all(racers)
      .flat_map(move |v| sync(move || (v, read.iter().all(|f| f.load(Ordering::SeqCst)))));
    assert_eq!(success(d, d.run(eff)), (5, true), "{d:?}");
  });
}

#[test]
fn a_loser_with_a_slow_finalizer_holds_the_result_back() {
  on_both(|d| {
    let (go, fin) = (counter(), flag());
    // The finalizer itself sleeps (it runs masked), so the race must wait for it.
    let racer: Effect<u8> = loser(&go, sleep(50 * MS).then(set(&fin)));
    let eff = then_read(racer.race(once_started(&go, 1, succeed(1))), &fin);
    assert_eq!(success(d, d.run(eff)), (1, true), "{d:?}");
  });
}

#[test]
fn interrupting_the_race_interrupts_and_awaits_every_racer() {
  on_both(|d| {
    let (go, a, b) = (counter(), flag(), flag());
    let race: Effect<u8> = loser(&go, set(&a)).race(loser(&go, set(&b)));
    let go2 = Arc::clone(&go);
    let eff: Effect<bool> = race
      .fork()
      .flat_map(move |h| once_started(&go2, 2, h.interrupt::<Never, _>()))
      .map({
        let (a, b) = (Arc::clone(&a), Arc::clone(&b));
        move |exit| {
          matches!(exit, Exit::Failure(Cause::Interrupt(_)))
            && a.load(Ordering::SeqCst)
            && b.load(Ordering::SeqCst)
        }
      });
    assert!(success(d, d.run(eff)), "{d:?}");
  });
}

// ── AC3: two failures produce Cause::Both ─────────────────────────────────────────────────

#[test]
fn race_two_failures_produce_both_in_input_order() {
  on_both(|d| {
    let eff = fail(1).race(fail_after(5 * MS, 2));
    assert_eq!(
      failure(d, d.run(eff)),
      Cause::both(Cause::fail(1), Cause::fail(2)),
      "{d:?}"
    );
    // Completion order does not change the shape: left is always `self`.
    let eff = fail_after(5 * MS, 1).race(fail(2));
    assert_eq!(
      failure(d, d.run(eff)),
      Cause::both(Cause::fail(1), Cause::fail(2)),
      "{d:?}"
    );
  });
}

#[test]
fn race_all_failures_fold_into_both() {
  on_both(|d| {
    let eff = race_all([fail_after(10 * MS, 1), fail(2), die("three")]);
    assert_eq!(
      failure(d, d.run(eff)),
      Cause::both(
        Cause::both(Cause::fail(1), Cause::fail(2)),
        Cause::die("three")
      ),
      "{d:?}"
    );
  });
}

// ── race_first: the first completion wins ─────────────────────────────────────────────────

#[test]
fn race_first_takes_the_first_completion_even_a_failure() {
  on_both(|d| {
    let (go, fin) = (counter(), flag());
    let eff = once_started(&go, 1, fail(4)).race_first(loser(&go, set(&fin)));
    assert_eq!(failure(d, d.run(eff)), Cause::fail(4), "{d:?}");
    assert!(is_set(&fin), "{d:?}: loser finalized");

    let eff: Effect<u8, u8> = after(5 * MS, 3).race_first(fail_after(HOUR, 1));
    assert_eq!(success(d, d.run(eff)), 3, "{d:?}");
  });
}

// ── AC4: timeout(10s) fires at virtual 10 s in < 50 ms wall time ──────────────────────────

#[test]
fn timeout_fires_at_virtual_ten_seconds_fast() {
  let wall = Instant::now();
  let exec = TestExecutor::new();
  let t0 = exec.now();
  let fin = flag();
  let eff: Effect<Option<u8>> = after(HOUR, 1).ensuring(set(&fin)).timeout(TEN_S);
  let h = eff.fork_root(Arc::new(exec.clone()));
  exec.run_until_stalled();
  exec.advance(TEN_S - MS);
  assert!(!h.is_done(), "not before the deadline");
  exec.advance(MS);
  assert!(h.is_done(), "done at the deadline");
  assert_eq!(exec.now() - t0, TEN_S, "virtual clock at exactly 10 s");
  assert!(
    is_set(&fin),
    "the timed-out effect was interrupted and finalized"
  );
  assert!(matches!(take_ready(h.join_future()), Exit::Success(None)));
  let elapsed = wall.elapsed();
  assert!(elapsed < 50 * MS, "wall time {elapsed:?} >= 50 ms");
}

#[test]
fn timeout_returns_some_when_the_effect_finishes_in_time() {
  on_both(|d| {
    let eff: Effect<Option<u8>> = after(10 * MS, 5).timeout(HOUR);
    assert_eq!(success(d, d.run(eff)), Some(5), "{d:?}");
    let eff: Effect<Option<u8>> = after(HOUR, 5).timeout(10 * MS);
    assert_eq!(success(d, d.run(eff)), None, "{d:?}");
  });
}

#[test]
fn timeout_reraises_a_failure_before_the_deadline() {
  on_both(|d| {
    let eff = fail_after(5 * MS, 6).timeout(HOUR);
    assert_eq!(failure(d, d.run(eff)), Cause::fail(6), "{d:?}");
  });
}

#[test]
fn timeout_fail_fails_with_the_callers_error() {
  on_both(|d| {
    let eff: Effect<u8, &str> = after(HOUR, 1).timeout_fail(10 * MS, || "too slow");
    assert_eq!(failure(d, d.run(eff)), Cause::fail("too slow"), "{d:?}");
    let eff: Effect<u8, &str> = after(5 * MS, 1).timeout_fail(HOUR, || "too slow");
    assert_eq!(success(d, d.run(eff)), 1, "{d:?}");
  });
}

// ── AC5: a masked child delays the timeout ────────────────────────────────────────────────

#[test]
fn a_masked_child_delays_the_timeout() {
  let exec = TestExecutor::new();
  let t0 = exec.now();
  let done = Arc::new(AtomicUsize::new(0));
  let done2 = Arc::clone(&done);
  let masked: Effect<u8> = sleep(2 * TEN_S)
    .flat_map(move |()| {
      sync(move || {
        done2.fetch_add(1, Ordering::SeqCst);
        1
      })
    })
    .uninterruptible();
  let h = masked.timeout(TEN_S).fork_root(Arc::new(exec.clone()));
  exec.run_until_stalled();
  exec.advance(TEN_S);
  assert!(
    !h.is_done(),
    "the deadline passed but the masked child is still running"
  );
  exec.advance(TEN_S - MS);
  assert!(!h.is_done(), "still waiting on the masked child");
  exec.advance(MS);
  assert!(h.is_done(), "done once the masked child completed");
  assert_eq!(exec.now() - t0, 2 * TEN_S);
  assert_eq!(
    done.load(Ordering::SeqCst),
    1,
    "the masked child ran to completion"
  );
  // The timer completed first, so the outcome is still the timeout.
  assert!(matches!(take_ready(h.join_future()), Exit::Success(None)));
}
