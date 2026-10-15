//! `c-zip-par-all` acceptance tests for `id_effect::{zip_par, all, for_each_par}`.
//!
//! Every behavioural test runs on a multi-thread `TokioExecutor` **and** on the deterministic
//! `TestExecutor` through [`on_both`]. On the test executor the root fiber is driven with
//! `run_until_stalled`, and the virtual clock is advanced only while the root is not done.

use std::future::Future;
use std::pin::pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use id_effect::Never;
use id_effect::{Cause, Exit};
use id_effect::{
  Concurrency, Effect, Executor, TestExecutor, TokioExecutor, all, die, fail, for_each_par, sleep,
  succeed, suspend, sync, yield_fiber, zip_par, zip_with_par,
};

const HOUR: Duration = Duration::from_secs(3600);

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
    self.timed(eff).0
  }

  /// Runs `eff` as a root fiber; returns its exit and how long it took (wall-clock time on
  /// tokio, virtual time on the test executor).
  fn timed<A: Send + 'static, E: Send + 'static>(
    self,
    eff: Effect<A, E>,
  ) -> (Exit<A, E>, Duration) {
    match self {
      Driver::Tokio => {
        let rt = tokio_rt();
        let t0 = Instant::now();
        let h = eff.fork_root(Arc::new(TokioExecutor::new(rt.handle().clone())));
        let exit = rt.block_on(async {
          tokio::time::timeout(Duration::from_secs(30), h.join_future())
            .await
            .expect("root fiber timed out on tokio")
        });
        (exit, t0.elapsed())
      }
      Driver::Test => {
        let exec = TestExecutor::new();
        let t0 = exec.now();
        let h = eff.fork_root(Arc::new(exec.clone()));
        exec.run_until_stalled();
        while !h.is_done() {
          let deadline = exec
            .next_timer_deadline()
            .expect("root fiber stalled with no timer pending");
          exec.advance(deadline.saturating_duration_since(exec.now()));
        }
        let elapsed = exec.now().saturating_duration_since(t0);
        (take_ready(h.join_future()), elapsed)
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

fn counter() -> Arc<AtomicUsize> {
  Arc::new(AtomicUsize::new(0))
}

fn get(c: &Arc<AtomicUsize>) -> usize {
  c.load(Ordering::SeqCst)
}

fn bump<E: Send + 'static>(c: &Arc<AtomicUsize>) -> Effect<(), E> {
  let c = Arc::clone(c);
  sync(move || {
    c.fetch_add(1, Ordering::SeqCst);
  })
}

/// Yields until `pred` holds (so a forked sibling has reached a known point).
fn wait_until<E: Send + 'static>(pred: impl Fn() -> bool + Send + Sync + 'static) -> Effect<(), E> {
  fn go<E: Send + 'static>(pred: Arc<dyn Fn() -> bool + Send + Sync>) -> Effect<(), E> {
    suspend(move || {
      if pred() {
        succeed(())
      } else {
        yield_fiber().flat_map(move |()| go(pred))
      }
    })
  }
  go(Arc::new(pred))
}

fn reached<E: Send + 'static>(c: &Arc<AtomicUsize>, n: usize) -> Effect<(), E> {
  let c = Arc::clone(c);
  wait_until(move || c.load(Ordering::SeqCst) >= n)
}

/// An effect that counts itself in and out of `in_flight` and records the high-water mark.
fn tracked(
  i: usize,
  in_flight: &Arc<AtomicUsize>,
  high_water: &Arc<AtomicUsize>,
) -> Effect<usize, Never> {
  let (inc, hw, dec) = (
    Arc::clone(in_flight),
    Arc::clone(high_water),
    Arc::clone(in_flight),
  );
  sync(move || {
    let now = inc.fetch_add(1, Ordering::SeqCst) + 1;
    hw.fetch_max(now, Ordering::SeqCst);
  })
  .then(sleep(Duration::from_millis(5 + (i as u64 % 3) * 5)))
  .then(sync(move || {
    dec.fetch_sub(1, Ordering::SeqCst);
    i
  }))
}

// ── AC1: zip_par over 2×50 ms takes < 80 ms ──────────────────────────────────────────────

/// Checks that two 50 ms branches overlapped.
///
/// On the test executor the measurement is virtual time, so the `< 80 ms` bound is exact and
/// deterministic. On tokio a wall-clock upper bound flakes under machine load (it once took
/// 99.9 ms at load ~50), so overlap is proven structurally instead: each branch waits at a
/// rendezvous until both have started (it could never complete if they ran one after the
/// other), and the wall clock only checks that the sleeps were honoured.
fn assert_overlapped(d: Driver, took: Duration, ms50: Duration) {
  assert!(took >= ms50, "{d:?}: took {took:?}");
  if let Driver::Test = d {
    assert!(took < Duration::from_millis(80), "{d:?}: took {took:?}");
  }
}

/// Bumps `c`, then waits until `n` branches have bumped it.
fn rendezvous(c: &Arc<AtomicUsize>, n: usize) -> Effect<(), Never> {
  bump::<Never>(c).then(reached(c, n))
}

#[test]
fn zip_par_over_two_50ms_sleeps_takes_under_80ms() {
  let _ = tokio_rt(); // runtime start-up is not part of the measurement
  on_both(|d| {
    let ms50 = Duration::from_millis(50);
    let met = counter();
    let eff = zip_par(
      rendezvous(&met, 2).then(sleep(ms50)).then(succeed(1_u8)),
      rendezvous(&met, 2).then(sleep(ms50)).then(succeed("two")),
    );
    let (exit, took) = d.timed(eff);
    assert!(matches!(exit, Exit::Success((1, "two"))), "{d:?}: {exit:?}");
    assert_overlapped(d, took, ms50);

    let met = counter();
    let eff = zip_with_par(
      rendezvous(&met, 2).then(sleep(ms50)).then(succeed(20_u32)),
      rendezvous(&met, 2).then(sleep(ms50)).then(succeed(22_u32)),
      |a, b| a + b,
    );
    let (exit, took) = d.timed(eff);
    assert!(matches!(exit, Exit::Success(42)), "{d:?}: {exit:?}");
    assert_overlapped(d, took, ms50);
  });
}

// ── AC2: failing fast interrupts siblings ────────────────────────────────────────────────

#[test]
fn zip_par_failure_interrupts_the_other_side_on_either_side() {
  on_both(|d| {
    for fail_left in [false, true] {
      let (go, fin) = (counter(), counter());
      let slow: Effect<u32, &'static str> = bump(&go)
        .then(sleep(HOUR))
        .then(succeed(1))
        .ensuring(bump(&fin));
      let failing: Effect<u32, &'static str> = reached(&go, 1).then(fail("boom"));
      let eff = if fail_left {
        zip_par(failing, slow).map(|(a, b)| a + b)
      } else {
        zip_par(slow, failing).map(|(a, b)| a + b)
      };
      let fin2 = Arc::clone(&fin);
      let eff = eff
        .exit::<Never>()
        .flat_map(move |ex| sync(move || (ex, get(&fin2))));
      let (Exit::Success((ex, fin_at_return)), took) = d.timed(eff) else {
        panic!("{d:?}: root failed")
      };
      assert!(
        matches!(ex, Exit::Failure(Cause::Fail("boom"))),
        "{d:?} fail_left={fail_left}: a lone failure stays exactly that cause: {ex:?}"
      );
      assert_eq!(
        fin_at_return, 1,
        "{d:?}: sibling finalizer ran before the result"
      );
      assert!(took < HOUR, "{d:?}: the hour-long sibling was interrupted");
    }
  });
}

#[test]
fn all_failure_interrupts_in_flight_and_starts_no_further_children() {
  on_both(|d| {
    let (started, fin) = (counter(), counter());
    // The failing child counts itself too, so `reached(2)` is "it and its sibling started".
    let mut effs: Vec<Effect<u32, &'static str>> = vec![
      bump(&started).then(reached(&started, 2)).then(fail("boom")),
      bump(&started)
        .then(sleep(HOUR))
        .then(succeed(1))
        .ensuring(bump(&fin)),
    ];
    effs.extend((0..3).map(|_| bump(&started).then(succeed(2))));
    let exit = d.run(all(effs, Concurrency::Bounded(2)));
    assert!(
      matches!(exit, Exit::Failure(Cause::Fail("boom"))),
      "{d:?}: {exit:?}"
    );
    assert_eq!(
      get(&started),
      2,
      "{d:?}: no child started after the failure"
    );
    assert_eq!(
      get(&fin),
      1,
      "{d:?}: the in-flight sibling was interrupted and finalized"
    );
  });
}

// Ruling (execution-log follow-up e): genuine sibling failures combine into `Cause::Both`,
// interrupts of siblings stopped by the first failure are dropped.

#[test]
fn a_genuine_sibling_failure_is_combined_with_cause_both() {
  on_both(|d| {
    let go = counter();
    let first: Effect<u32, &'static str> = reached(&go, 1).then(fail("first"));
    let second: Effect<u32, &'static str> = bump(&go)
      .then(sleep(Duration::from_millis(20)))
      .then(fail("second"))
      .uninterruptible();
    let exit = d.run(all([first, second], Concurrency::Unbounded));
    assert!(
      matches!(&exit, Exit::Failure(Cause::Both(l, r))
        if matches!(&**l, Cause::Fail("first")) && matches!(&**r, Cause::Fail("second"))),
      "{d:?}: {exit:?}"
    );
  });
}

#[test]
fn a_sibling_finalizer_defect_is_kept_without_its_interrupt() {
  on_both(|d| {
    let go = counter();
    let first: Effect<u32, &'static str> = reached(&go, 1).then(fail("first"));
    let second: Effect<u32, &'static str> = bump(&go)
      .then(sleep(HOUR))
      .then(succeed(2))
      .ensuring(die("finalizer died"));
    let exit = d.run(all([first, second], Concurrency::Unbounded));
    let Exit::Failure(Cause::Both(l, r)) = &exit else {
      panic!("{d:?}: {exit:?}")
    };
    assert!(matches!(&**l, Cause::Fail("first")), "{d:?}: {exit:?}");
    assert!(
      r.contains(&|c| matches!(c, Cause::Die(m) if m.message() == "finalizer died")),
      "{d:?}: {exit:?}"
    );
    assert!(
      !r.contains(&|c| matches!(c, Cause::Interrupt(_))),
      "{d:?}: sibling interrupts are dropped: {exit:?}"
    );
  });
}

// ── AC3: all(_, Bounded(2)) never has more than 2 in flight ──────────────────────────────

#[test]
fn all_bounded_2_never_has_more_than_2_in_flight() {
  on_both(|d| {
    for n in [1_usize, 2, 9] {
      let (in_flight, high_water) = (counter(), counter());
      let effs: Vec<_> = (0..n)
        .map(|i| tracked(i, &in_flight, &high_water))
        .collect();
      let Exit::Success(v) = d.run(all(effs, Concurrency::Bounded(2))) else {
        panic!("{d:?}")
      };
      assert_eq!(v, (0..n).collect::<Vec<_>>(), "{d:?}");
      assert_eq!(
        get(&high_water),
        n.min(2),
        "{d:?} n={n}: at most (and at some point exactly) 2 in flight"
      );
      assert_eq!(get(&in_flight), 0, "{d:?}");
    }
  });
}

#[test]
fn for_each_par_bounded_2_never_has_more_than_2_in_flight() {
  on_both(|d| {
    let (in_flight, high_water) = (counter(), counter());
    let eff = for_each_par(
      0..9,
      |i| tracked(i, &in_flight, &high_water),
      Concurrency::Bounded(2),
    );
    assert!(matches!(d.run(eff), Exit::Success(v) if v == (0..9).collect::<Vec<_>>()));
    assert_eq!(get(&high_water), 2, "{d:?}");
  });
}

// ── AC4: for_each_par keeps order ────────────────────────────────────────────────────────

#[test]
fn for_each_par_keeps_input_order_not_completion_order() {
  on_both(|d| {
    for c in [
      Concurrency::Unbounded,
      Concurrency::Inherit,
      Concurrency::Bounded(3),
      Concurrency::Sequential,
    ] {
      let done = Arc::new(Mutex::new(Vec::new()));
      let eff = for_each_par(
        0..8_u64,
        |i| {
          let done = Arc::clone(&done);
          sleep::<Never, _>(Duration::from_millis(3 * (8 - i))).then(sync(move || {
            done.lock().unwrap().push(i);
            i * 10
          }))
        },
        c,
      );
      let Exit::Success(v) = d.run(eff) else {
        panic!("{d:?} {c:?}")
      };
      assert_eq!(v, (0..8).map(|i| i * 10).collect::<Vec<_>>(), "{d:?} {c:?}");
      if matches!(c, Concurrency::Unbounded | Concurrency::Inherit) && matches!(d, Driver::Test) {
        assert_eq!(
          *done.lock().unwrap(),
          (0..8).rev().collect::<Vec<_>>(),
          "{d:?} {c:?}: completion order differs from input order"
        );
      }
    }
    let empty = for_each_par(
      Vec::<u8>::new(),
      succeed::<u8, Never, _>,
      Concurrency::Unbounded,
    );
    assert!(matches!(d.run(empty), Exit::Success(v) if v.is_empty()));
  });
}

// ── AC5: Sequential matches a fold ───────────────────────────────────────────────────────

fn step(i: u32, log: &Arc<Mutex<Vec<u32>>>) -> Effect<u32, &'static str> {
  let log = Arc::clone(log);
  sync(move || {
    log.lock().unwrap().push(i);
    i * 2
  })
  .flat_map(move |v| if i == 3 { fail("stop") } else { succeed(v) })
}

fn fold(effs: Vec<Effect<u32, &'static str>>) -> Effect<Vec<u32>, &'static str> {
  effs.into_iter().fold(succeed(Vec::new()), |acc, e| {
    acc.flat_map(move |mut v| {
      e.map(move |a| {
        v.push(a);
        v
      })
    })
  })
}

#[test]
fn sequential_matches_a_fold() {
  on_both(|d| {
    for n in [0_u32, 3, 6] {
      let (log_all, log_fold, log_each) = (
        Arc::new(Mutex::new(Vec::new())),
        Arc::new(Mutex::new(Vec::new())),
        Arc::new(Mutex::new(Vec::new())),
      );
      let via_all = d.run(all(
        (0..n).map(|i| step(i, &log_all)),
        Concurrency::Sequential,
      ));
      let via_each = d.run(for_each_par(
        0..n,
        |i| step(i, &log_each),
        Concurrency::Sequential,
      ));
      let via_fold = d.run(fold((0..n).map(|i| step(i, &log_fold)).collect()));
      for got in [&via_all, &via_each] {
        match (got, &via_fold) {
          (Exit::Success(a), Exit::Success(b)) => assert_eq!(a, b, "{d:?} n={n}"),
          (Exit::Failure(Cause::Fail(a)), Exit::Failure(Cause::Fail(b))) => {
            assert_eq!(a, b, "{d:?} n={n}");
          }
          other => panic!("{d:?} n={n}: {other:?}"),
        }
      }
      let expected = log_fold.lock().unwrap().clone();
      assert_eq!(*log_all.lock().unwrap(), expected, "{d:?} n={n}");
      assert_eq!(*log_each.lock().unwrap(), expected, "{d:?} n={n}");
    }
  });
}

#[test]
fn sequential_runs_one_at_a_time() {
  on_both(|d| {
    let (in_flight, high_water) = (counter(), counter());
    let effs: Vec<_> = (0..5)
      .map(|i| tracked(i, &in_flight, &high_water))
      .collect();
    assert!(
      matches!(d.run(all(effs, Concurrency::Sequential)), Exit::Success(v) if v == [0, 1, 2, 3, 4])
    );
    assert_eq!(get(&high_water), 1, "{d:?}");
  });
}

/// Interrupting the caller interrupts every child (and their finalizers have run).
#[test]
fn interrupting_the_caller_interrupts_the_children() {
  on_both(|d| {
    let (go, fin) = (counter(), counter());
    let kids: Vec<Effect<u32, Never>> = (0..3)
      .map(|_| {
        bump(&go)
          .then(sleep(HOUR))
          .then(succeed(1))
          .ensuring(bump(&fin))
      })
      .collect();
    let par = all(kids, Concurrency::Unbounded);
    let go2 = Arc::clone(&go);
    let eff = par.fork().flat_map(move |h| {
      wait_until(move || go2.load(Ordering::SeqCst) == 3).flat_map(move |()| h.interrupt())
    });
    let (exit, took) = d.timed(eff);
    assert!(
      matches!(exit, Exit::Success(Exit::Failure(Cause::Interrupt(_)))),
      "{d:?}: {exit:?}"
    );
    assert_eq!(get(&fin), 3, "{d:?}: every child was finalized");
    assert!(took < HOUR, "{d:?}");
  });
}
