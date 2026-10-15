//! `c-join-binds` acceptance tests: the parallel join `~(a, b, ..)` inside `effect!`.
//!
//! - it fails fast and interrupts the siblings (their finalizers run before the join fails);
//! - its outcome has `zip_par`'s error shape: the first failure is the `~`'s outcome (typed
//!   error, defect or interrupt), genuine sibling failures are not surfaced;
//! - arity 2 to 8, values in input order, branches run concurrently.
//!
//! Every behavioural test runs on a multi-thread `TokioExecutor` **and** on the deterministic
//! `TestExecutor` (drivers as in `tests/par.rs`).

use std::future::Future;
use std::pin::pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use id_effect::Never;
use id_effect::{Cause, Exit};
use id_effect::{
  Effect, Executor, HNil, TestExecutor, TokioExecutor, caps, die, effect, fail, hlist, need, sleep,
  succeed, suspend, sync, yield_fiber, zip_par,
};

const HOUR: Duration = Duration::from_secs(3600);
const MS50: Duration = Duration::from_millis(50);

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

fn bump<E: Send + 'static, R: Send + Sync + 'static>(c: &Arc<AtomicUsize>) -> Effect<(), E, R> {
  let c = Arc::clone(c);
  sync(move || {
    c.fetch_add(1, Ordering::SeqCst);
  })
}

/// Yields until `c` reached `n` (so a forked sibling has reached a known point).
fn reached<E: Send + 'static>(c: &Arc<AtomicUsize>, n: usize) -> Effect<(), E> {
  let c = Arc::clone(c);
  suspend(move || {
    if c.load(Ordering::SeqCst) >= n {
      succeed(())
    } else {
      let c2 = Arc::clone(&c);
      yield_fiber().flat_map(move |()| reached(&c2, n))
    }
  })
}

fn after<A: Send + 'static>(d: Duration, a: A) -> Effect<A, Never> {
  sleep(d).then(succeed(a))
}

fn ok(v: u64) -> Effect<u64, Never> {
  succeed(v)
}

// ── arity, order and concurrency ─────────────────────────────────────────────────────────

fn join2() -> Effect<(u8, &'static str), Never> {
  effect!({ ~(after(MS50, 1_u8), after(MS50, "two")) })
}

#[allow(clippy::type_complexity)]
fn join8() -> Effect<(u8, u16, u32, u64, i8, i16, i32, &'static str), Never> {
  effect!({
    ~(
      after(MS50, 1_u8),
      after(MS50, 2_u16),
      after(MS50, 3_u32),
      after(MS50, 4_u64),
      after(MS50, 5_i8),
      after(MS50, 6_i16),
      after(MS50, 7_i32),
      after(MS50, "eight"),
    )
  })
}

#[test]
fn a_join_runs_its_branches_concurrently_and_keeps_input_order() {
  let _ = tokio_rt(); // runtime start-up is not part of the measurement
  on_both(|d| {
    let (exit, took) = d.timed(join2());
    assert!(matches!(exit, Exit::Success((1, "two"))), "{d:?}: {exit:?}");
    assert!(
      took >= MS50 && took < Duration::from_millis(80),
      "{d:?}: took {took:?}"
    );

    let (exit, took) = d.timed(join8());
    assert!(
      matches!(exit, Exit::Success((1, 2, 3, 4, 5, 6, 7, "eight"))),
      "{d:?}: {exit:?}"
    );
    assert!(
      took >= MS50 && took < Duration::from_millis(80),
      "{d:?}: took {took:?}"
    );
  });
}

#[test]
fn every_arity_from_2_to_8_binds_and_the_body_continues() {
  on_both(|d| {
    let eff: Effect<u64, Never> = effect!({
      let (a, b) = ~(ok(1), ok(2));
      let (c, d, e) = ~(ok(3), ok(4), ok(5));
      let (f, g, h, i) = ~(ok(6), ok(7), ok(8), ok(9));
      let (j, ..) = ~(ok(10), ok(0), ok(0), ok(0), ok(0));
      let (k, ..) = ~(
        ok(11),
        ok(0),
        ok(0),
        ok(0),
        ok(0),
        ok(0),
      );
      let (l, ..) = ~(
        ok(12),
        ok(0),
        ok(0),
        ok(0),
        ok(0),
        ok(0),
        ok(0),
      );
      // A single parenthesised effect stays a plain bind.
      let m = ~(succeed::<u64, Never, HNil>(13));
      a + b + c + d + e + f + g + h + i + j + k + l + m
    });
    assert_eq!(d.run(eff), Exit::Success((1..=13).sum()), "{d:?}");
  });
}

// ── fail fast, siblings interrupted ──────────────────────────────────────────────────────

#[test]
fn the_first_failure_interrupts_the_siblings_and_their_finalizers_run() {
  on_both(|d| {
    for fail_left in [false, true] {
      let (go, fin, cont) = (counter(), counter(), counter());
      let slow: Effect<u32, &'static str> = bump(&go)
        .then(sleep(HOUR))
        .then(succeed(1))
        .ensuring(bump(&fin));
      let failing: Effect<u32, &'static str> = reached(&go, 1).then(fail("boom"));
      let cont2 = Arc::clone(&cont);
      let eff: Effect<u32, &'static str> = if fail_left {
        effect!({
          let (a, b) = ~(failing, slow);
          cont2.fetch_add(1, Ordering::SeqCst);
          a + b
        })
      } else {
        effect!({
          let (a, b) = ~(slow, failing);
          cont2.fetch_add(1, Ordering::SeqCst);
          a + b
        })
      };
      let (exit, took) = d.timed(eff);
      assert_eq!(
        exit,
        Exit::Failure(Cause::Fail("boom")),
        "{d:?} left={fail_left}"
      );
      assert_eq!(
        get(&fin),
        1,
        "{d:?}: the sibling's finalizer ran before the join failed"
      );
      assert_eq!(get(&cont), 0, "{d:?}: the body did not continue");
      assert!(
        took < Duration::from_secs(5),
        "{d:?}: failed fast, took {took:?}"
      );
    }
  });
}

#[test]
fn interrupting_the_body_interrupts_every_branch() {
  on_both(|d| {
    let (go, fin) = (counter(), counter());
    let branch = |go: &Arc<AtomicUsize>, fin: &Arc<AtomicUsize>| -> Effect<u32, Never> {
      bump(go)
        .then(sleep(HOUR))
        .then(succeed(1))
        .ensuring(bump(fin))
    };
    let (a, b) = (branch(&go, &fin), branch(&go, &fin));
    let joined: Effect<u32, Never> = effect!({
      let (a, b) = ~(a, b);
      a + b
    });
    let winner = reached(&go, 2).then(succeed(7));
    let (exit, took) = d.timed(joined.race(winner));
    assert_eq!(exit, Exit::Success(7), "{d:?}");
    assert_eq!(get(&fin), 2, "{d:?}: both branches' finalizers ran");
    assert!(took < Duration::from_secs(5), "{d:?}: took {took:?}");
  });
}

// ── error shape: zip_par's first failure ─────────────────────────────────────────────────

/// Two genuine failures: "first" fails at once, "second" fails later and cannot be interrupted.
fn two_failures(go: &Arc<AtomicUsize>) -> (Effect<u32, &'static str>, Effect<u32, &'static str>) {
  let first = reached(go, 1).then(fail("first"));
  let second = bump(go)
    .then(sleep(Duration::from_millis(20)))
    .then(fail("second"))
    .uninterruptible();
  (first, second)
}

#[test]
fn the_join_surfaces_the_first_failure_of_zip_pars_cause() {
  on_both(|d| {
    let go = counter();
    let (first, second) = two_failures(&go);
    let exit = d.run(zip_par(first, second));
    let Exit::Failure(Cause::Both(l, r)) = &exit else {
      panic!("{d:?}: zip_par keeps both genuine failures: {exit:?}")
    };
    assert_eq!(
      (&**l, &**r),
      (&Cause::Fail("first"), &Cause::Fail("second")),
      "{d:?}"
    );

    let go = counter();
    let (first, second) = two_failures(&go);
    let eff: Effect<u32, &'static str> = effect!({
      let (a, b) = ~(first, second);
      a + b
    });
    assert_eq!(
      d.run(eff),
      Exit::Failure(Cause::Fail("first")),
      "{d:?}: the `~` outcome is zip_par's leftmost (first) failure"
    );
  });
}

#[test]
fn a_first_defect_is_the_outcome_even_when_a_sibling_fails_with_a_typed_error() {
  on_both(|d| {
    let go = counter();
    let first: Effect<u32, &'static str> = reached(&go, 1).then(die("first died"));
    let second: Effect<u32, &'static str> = bump(&go)
      .then(sleep(Duration::from_millis(20)))
      .then(fail("second"))
      .uninterruptible();
    let eff: Effect<u32, &'static str> = effect!({
      let (a, b) = ~(first, second);
      a + b
    });
    match d.run(eff) {
      Exit::Failure(Cause::Die(defect)) => assert_eq!(defect.message(), "first died", "{d:?}"),
      other => panic!("{d:?}: expected the first defect, got {other:?}"),
    }
  });
}

// ── typing: per-branch error widening and capabilities ──────────────────────────────────

#[derive(Debug, PartialEq, Eq, Hash)]
enum AppError {
  Db(&'static str),
  Io(u16),
}

impl From<&'static str> for AppError {
  fn from(e: &'static str) -> Self {
    AppError::Db(e)
  }
}

impl From<u16> for AppError {
  fn from(e: u16) -> Self {
    AppError::Io(e)
  }
}

#[test]
fn each_branch_error_widens_into_the_bodys_error() {
  on_both(|d| {
    let ok: Effect<u8, AppError> = effect!({
      let (a, b, c) = ~(
        succeed::<u8, Never, HNil>(1),
        succeed::<u8, &'static str, HNil>(2),
        succeed::<u8, u16, HNil>(3),
      );
      a + b + c
    });
    assert_eq!(d.run(ok), Exit::Success(6), "{d:?}");

    let io: Effect<u8, AppError> = effect!({
      let (a, b) = ~(succeed::<u8, &'static str, HNil>(1), fail::<u8, u16, HNil>(404));
      a + b
    });
    assert_eq!(
      d.run(io),
      Exit::Failure(Cause::Fail(AppError::Io(404))),
      "{d:?}"
    );
  });
}

#[derive(Debug)]
struct Db(u32);
#[derive(Debug)]
struct Cache(u32);
#[derive(Debug)]
struct Clock(u32);

type AppCaps = caps!(Db, Cache, Clock);

fn read_db() -> Effect<u32, Never, caps!(Db)> {
  effect!({ ~Db.0 })
}

fn read_cache() -> Effect<u32, Never, caps!(Cache)> {
  effect!({ ~Cache.0 })
}

#[test]
fn branches_run_over_parts_of_the_bodys_environment() {
  on_both(|d| {
    let eff: Effect<u32, Never, AppCaps> = effect!({
      let (db, cache, clock) = ~(
        read_db(),
        read_cache(),
        need::<Clock>().map::<_, Never, _, _>(|c| c.0).same_env(),
      );
      db * 100 + cache * 10 + clock
    });
    let exit = d.run(eff.provide_env(hlist![Db(1), Cache(2), Clock(3)]));
    assert_eq!(exit, Exit::Success(123), "{d:?}");
  });
}
