//! `k-fiber-tree` acceptance tests for `id_effect`: the fiber tree, the one-shot exit
//! slot, fork / fork_daemon / join / interrupt, masks, finalizers and `fork_all`.
//!
//! Every behavioural test runs on a multi-thread `TokioExecutor` **and** on the deterministic
//! `TestExecutor` through [`on_both`]. On the test executor the root fiber is driven with
//! `run_until_stalled`, and the virtual clock is advanced only while the root is not done.

use std::cell::Cell;
use std::future::Future;
use std::pin::pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use id_effect::{
  BoxFuture, Concurrency, Effect, Executor, ExitKind, FiberHandle, LocalBoxFuture, NotLocal,
  TestExecutor, TokioExecutor, acquire_use_release, fail, fork_all, sleep, succeed, suspend, sync,
  uninterruptible_mask, yield_fiber,
};
use id_effect::{Cause, Exit};
use id_effect::{FiberId, Never};

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

fn counter() -> Arc<AtomicUsize> {
  Arc::new(AtomicUsize::new(0))
}

fn flag() -> Arc<AtomicBool> {
  Arc::new(AtomicBool::new(false))
}

fn bump<E: Send + 'static>(c: &Arc<AtomicUsize>) -> Effect<(), E> {
  let c = Arc::clone(c);
  sync(move || {
    c.fetch_add(1, Ordering::SeqCst);
  })
}

fn set<E: Send + 'static>(f: &Arc<AtomicBool>) -> Effect<(), E> {
  let f = Arc::clone(f);
  sync(move || f.store(true, Ordering::SeqCst))
}

fn get(c: &Arc<AtomicUsize>) -> usize {
  c.load(Ordering::SeqCst)
}

/// Yields until `pred` holds (so a forked child has reached a known point).
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

fn started<E: Send + 'static>(f: &Arc<AtomicBool>) -> Effect<(), E> {
  let f = Arc::clone(f);
  wait_until(move || f.load(Ordering::SeqCst))
}

fn die_message<A, E>(exit: &Exit<A, E>) -> Option<String> {
  match exit {
    Exit::Failure(Cause::Die(d)) => Some(d.message().to_owned()),
    _ => None,
  }
}

// ── AC2: join without Clone / Sync ────────────────────────────────────────────────────────

/// `Send` but neither `Clone` nor `Sync`.
struct NoClone(Cell<u8>);

#[test]
fn join_needs_neither_clone_nor_sync() {
  fn assert_send<T: Send>() {}
  assert_send::<FiberHandle<NoClone, NoClone>>();
  assert_send::<Effect<FiberHandle<NoClone, NoClone>, NoClone>>();
  on_both(|d| {
    let ok: Effect<NoClone, NoClone> = sync(|| NoClone(Cell::new(7)))
      .fork()
      .widen()
      .flat_map(FiberHandle::join);
    let Exit::Success(v) = d.run(ok) else {
      panic!("{d:?}: expected success")
    };
    assert_eq!(v.0.get(), 7);

    let err: Effect<u8, NoClone> = fail(NoClone(Cell::new(9)))
      .fork()
      .widen()
      .flat_map(FiberHandle::join);
    let Exit::Failure(Cause::Fail(e)) = d.run(err) else {
      panic!("{d:?}: expected typed failure")
    };
    assert_eq!(e.0.get(), 9);
  });
}

// ── AC3: interrupting a sleeping fiber ────────────────────────────────────────────────────

/// A child that marks `go`, sleeps for an hour, and bumps `fin` in its finalizer.
fn sleeper(go: &Arc<AtomicBool>, fin: &Arc<AtomicUsize>) -> Effect<(), Never> {
  set(go).then(sleep(HOUR)).ensuring(bump(fin))
}

#[test]
fn interrupt_stops_a_sleeping_fiber_and_runs_its_finalizer() {
  on_both(|d| {
    let (go, fin) = (flag(), counter());
    let eff = sleeper(&go, &fin).fork().flat_map({
      let go = Arc::clone(&go);
      move |h| {
        let id = h.id();
        started(&go)
          .then(sync(Instant::now))
          .flat_map(move |t0| h.interrupt().map(move |ex| (id, ex, t0.elapsed())))
      }
    });
    let Exit::Success((id, ex, latency)) = d.run(eff) else {
      panic!("{d:?}: root failed")
    };
    assert!(
      matches!(ex, Exit::Failure(Cause::Interrupt(i)) if i == id),
      "{d:?}: {ex:?}"
    );
    assert_eq!(get(&fin), 1, "{d:?}: ensuring finalizer ran");
    if let Driver::Tokio = d {
      println!("interrupt latency (tokio, sleeping child): {latency:?}");
      assert!(latency < Duration::from_millis(500), "latency {latency:?}");
    }
  });
}

#[test]
fn interrupt_on_the_test_executor_needs_no_advance() {
  let exec = TestExecutor::new();
  let (go, fin) = (flag(), counter());
  let eff: Effect<Exit<(), Never>> = sleeper(&go, &fin).fork().flat_map({
    let go = Arc::clone(&go);
    move |h| started(&go).then(h.interrupt())
  });
  let h = eff.fork_root(Arc::new(exec.clone()));
  exec.run_until_stalled();
  assert!(h.is_done(), "no advance needed");
  let Exit::Success(ex) = take_ready(h.join_future()) else {
    panic!("root failed")
  };
  assert!(matches!(ex, Exit::Failure(Cause::Interrupt(_))));
  assert_eq!(get(&fin), 1);
}

// ── AC4: a parent's exit interrupts and awaits its children ───────────────────────────────

#[test]
fn parent_exit_interrupts_and_awaits_live_children() {
  #[derive(Clone, Copy, Debug)]
  enum Tail {
    Succeed,
    Fail,
    Interrupted,
  }
  for tail in [Tail::Succeed, Tail::Fail, Tail::Interrupted] {
    on_both(|d| {
      let (go, fin) = (flag(), counter());
      let rest: Effect<u32, &'static str> = match tail {
        Tail::Succeed => succeed(1),
        Tail::Fail => fail("parent failed"),
        Tail::Interrupted => sleep(HOUR).then(succeed(1)),
      };
      let parent = sleeper(&go, &fin).fork().widen().flat_map({
        let go = Arc::clone(&go);
        move |_child| started(&go).then(rest)
      });
      let eff = parent.fork().flat_map({
        let go = Arc::clone(&go);
        move |ph| {
          let pctx = Arc::clone(ph.fiber());
          let exit = match tail {
            Tail::Interrupted => started(&go).then(ph.interrupt()),
            _ => ph.join().exit(),
          };
          exit.map(move |ex| (ExitKind::of(&ex), pctx.live_children(), pctx.is_done()))
        }
      });
      let Exit::Success((kind, live, done)) = d.run(eff) else {
        panic!("{d:?}/{tail:?}: root failed")
      };
      let want = match tail {
        Tail::Succeed => ExitKind::Success,
        Tail::Fail => ExitKind::Failure,
        Tail::Interrupted => ExitKind::Interrupt,
      };
      assert_eq!(kind, want, "{d:?}/{tail:?}");
      assert_eq!(
        live, 0,
        "{d:?}/{tail:?}: no live children when the joiner resumes"
      );
      assert_eq!(get(&fin), 1, "{d:?}/{tail:?}: child finalizer ran first");
      assert!(
        done,
        "{d:?}/{tail:?}: parent is done when its exit is observed"
      );
    });
  }
}

// ── AC5: fork_daemon outlives its parent ──────────────────────────────────────────────────

#[test]
fn fork_daemon_keeps_running_after_its_parent_exits() {
  on_both(|d| {
    let ran = flag();
    let parent = sleep(Duration::from_millis(200))
      .then(set(&ran))
      .fork_daemon();
    let eff = parent.fork().flat_map({
      let ran = Arc::clone(&ran);
      move |ph| {
        let pctx = Arc::clone(ph.fiber());
        ph.join().flat_map(move |daemon: FiberHandle<(), Never>| {
          assert!(pctx.is_done(), "parent has exited");
          assert!(daemon.fiber().parent().is_none(), "daemon has no parent");
          assert!(
            !daemon.is_done(),
            "daemon still running after its parent exited"
          );
          daemon.join().map(move |()| ran.load(Ordering::SeqCst))
        })
      }
    });
    assert!(matches!(d.run(eff), Exit::Success(true)), "{d:?}");

    // Contrast: a plain `fork` child of the same parent is interrupted when the parent exits.
    let attached = sleep::<Never, _>(Duration::from_millis(200)).fork();
    let eff = attached
      .fork()
      .flat_map(|ph| ph.join().flat_map(|child| child.join().exit()));
    assert!(
      matches!(
        d.run(eff),
        Exit::Success(Exit::Failure(Cause::Interrupt(_)))
      ),
      "{d:?}"
    );
  });
}

// ── AC6: masks ────────────────────────────────────────────────────────────────────────────

#[test]
fn uninterruptible_defers_the_interrupt_until_the_region_exits() {
  on_both(|d| {
    let (go, inside, after) = (flag(), counter(), counter());
    let child: Effect<(), Never> = set(&go)
      .then(sleep(Duration::from_millis(20)))
      .then(bump(&inside))
      .uninterruptible()
      .then(bump(&after));
    let eff = child.fork().flat_map({
      let go = Arc::clone(&go);
      move |h| started(&go).then(h.interrupt())
    });
    let Exit::Success(ex) = d.run(eff) else {
      panic!("{d:?}: root failed")
    };
    assert!(matches!(ex, Exit::Failure(Cause::Interrupt(_))), "{d:?}");
    assert_eq!(get(&inside), 1, "{d:?}: masked region completed");
    assert_eq!(get(&after), 0, "{d:?}: nothing ran after the region");
  });
}

#[test]
fn uninterruptible_mask_restore_is_interruptible_again() {
  on_both(|d| {
    let (go, masked, after) = (flag(), counter(), counter());
    let child: Effect<(), Never> = uninterruptible_mask({
      let (go, masked, after) = (Arc::clone(&go), Arc::clone(&masked), Arc::clone(&after));
      move |restore| {
        set(&go)
          .then(sleep(Duration::from_millis(20)))
          .then(bump(&masked))
          .then(restore.apply(sleep(HOUR)))
          .then(bump(&after))
      }
    });
    let eff = child.fork().flat_map({
      let go = Arc::clone(&go);
      move |h| started(&go).then(h.interrupt())
    });
    let Exit::Success(ex) = d.run(eff) else {
      panic!("{d:?}: root failed")
    };
    assert!(matches!(ex, Exit::Failure(Cause::Interrupt(_))), "{d:?}");
    assert_eq!(get(&masked), 1, "{d:?}: masked part completed");
    assert_eq!(get(&after), 0, "{d:?}: interrupted inside restore");
  });
}

#[test]
fn interruptible_reopens_an_uninterruptible_region() {
  on_both(|d| {
    let go = flag();
    let child: Effect<(), Never> = set(&go).then(sleep(HOUR).interruptible()).uninterruptible();
    let eff = child.fork().flat_map({
      let go = Arc::clone(&go);
      move |h| started(&go).then(h.interrupt())
    });
    assert!(
      matches!(
        d.run(eff),
        Exit::Success(Exit::Failure(Cause::Interrupt(_)))
      ),
      "{d:?}"
    );
  });
}

// ── AC7: acquire_use_release ──────────────────────────────────────────────────────────────

fn with_resource(
  log: &Arc<Mutex<Vec<ExitKind>>>,
  use_: Effect<u32, &'static str>,
) -> Effect<u32, &'static str> {
  let log = Arc::clone(log);
  acquire_use_release(
    sync(|| 5_u32),
    move |r| use_.map(move |v| v + r),
    move |_r, kind| sync(move || log.lock().unwrap().push(kind)),
  )
}

#[test]
fn acquire_use_release_releases_once_with_the_matching_exit_kind() {
  on_both(|d| {
    let log = Arc::new(Mutex::new(Vec::new()));
    let ok = d.run(with_resource(&log, succeed(1)));
    assert!(matches!(ok, Exit::Success(6)), "{d:?}");
    let failed = d.run(with_resource(&log, fail("bad")));
    assert!(matches!(failed, Exit::Failure(Cause::Fail("bad"))), "{d:?}");
    let died = d.run(with_resource(&log, sync(|| panic!("boom"))));
    assert_eq!(die_message(&died).as_deref(), Some("boom"), "{d:?}");

    let go = flag();
    let interrupted = with_resource(&log, set(&go).then(sleep(HOUR)).then(succeed(1)))
      .fork()
      .flat_map({
        let go = Arc::clone(&go);
        move |h| started(&go).then(h.interrupt())
      });
    let Exit::Success(ex) = d.run(interrupted) else {
      panic!("{d:?}: root failed")
    };
    assert!(matches!(ex, Exit::Failure(Cause::Interrupt(_))), "{d:?}");

    assert_eq!(
      *log.lock().unwrap(),
      [
        ExitKind::Success,
        ExitKind::Failure,
        ExitKind::Die,
        ExitKind::Interrupt
      ],
      "{d:?}"
    );
  });
}

#[test]
fn failed_acquire_releases_nothing() {
  on_both(|d| {
    let log = Arc::new(Mutex::new(Vec::new()));
    let eff = acquire_use_release(fail::<u32, _, _>("no"), succeed, {
      let log = Arc::clone(&log);
      move |_, kind| sync(move || log.lock().unwrap().push(kind))
    });
    assert!(matches!(d.run(eff), Exit::Failure(Cause::Fail("no"))));
    assert!(log.lock().unwrap().is_empty(), "{d:?}");
  });
}

#[test]
fn a_dying_finalizer_is_sequenced_after_the_original_cause() {
  on_both(|d| {
    let eff: Effect<(), &'static str> = fail("first").ensuring(sync(|| panic!("fin")));
    match d.run(eff) {
      Exit::Failure(Cause::Then(l, r)) => {
        assert!(matches!(*l, Cause::Fail("first")), "{d:?}");
        assert!(
          matches!(&*r, Cause::Die(x) if x.message() == "fin"),
          "{d:?}"
        );
      }
      other => panic!("{d:?}: expected Then(Fail, Die), got {other:?}"),
    }
  });
}

// ── panic at the fiber boundary ───────────────────────────────────────────────────────────

#[test]
fn a_panicking_child_dies_and_its_joiner_sees_the_defect() {
  on_both(|d| {
    let eff: Effect<u8, Never> = sync(|| -> u8 { panic!("boom") })
      .fork()
      .flat_map(FiberHandle::join);
    assert_eq!(die_message(&d.run(eff)).as_deref(), Some("boom"), "{d:?}");
  });
}

// ── AC8: fork_all ─────────────────────────────────────────────────────────────────────────

#[test]
fn fork_all_unbounded_collects_values_in_input_order() {
  on_both(|d| {
    let effs: Vec<Effect<u64, Never>> = (0..8_u64)
      .map(|i| sleep(Duration::from_millis(5 * (8 - i))).then(succeed(i)))
      .collect();
    let Exit::Success(v) = d.run(fork_all(effs, Concurrency::Unbounded)) else {
      panic!("{d:?}")
    };
    assert_eq!(v, (0..8).collect::<Vec<_>>(), "{d:?}");
    let empty: Vec<Effect<u8, Never>> = Vec::new();
    assert!(
      matches!(d.run(fork_all(empty, Concurrency::Inherit)), Exit::Success(v) if v.is_empty())
    );
  });
}

#[test]
fn fork_all_first_failure_interrupts_siblings_and_runs_their_finalizers_first() {
  on_both(|d| {
    let (go, fin) = (counter(), counter());
    let siblings = 4;
    let mut effs: Vec<Effect<u32, &'static str>> = (0..siblings)
      .map(|_| {
        bump(&go)
          .then(sleep(HOUR))
          .then(succeed(1))
          .ensuring(bump(&fin))
      })
      .collect();
    let go2 = Arc::clone(&go);
    effs.insert(
      2,
      wait_until(move || go2.load(Ordering::SeqCst) == siblings).then(fail("boom")),
    );
    let fin2 = Arc::clone(&fin);
    let eff = fork_all(effs, Concurrency::Unbounded)
      .exit::<Never>()
      .flat_map(move |ex| sync(move || (ex, get(&fin2))));
    let Exit::Success((ex, fin_at_return)) = d.run(eff) else {
      panic!("{d:?}: root failed")
    };
    assert!(
      matches!(ex, Exit::Failure(Cause::Fail("boom"))),
      "{d:?}: {ex:?}"
    );
    assert_eq!(
      fin_at_return, siblings,
      "{d:?}: every sibling finalizer ran before the result"
    );
  });
}

#[test]
fn fork_all_sequential_is_a_left_fold() {
  on_both(|d| {
    let order = Arc::new(Mutex::new(Vec::new()));
    let step = |i: u32, order: &Arc<Mutex<Vec<u32>>>| -> Effect<u32, &'static str> {
      let order = Arc::clone(order);
      sync(move || {
        order.lock().unwrap().push(i);
        i
      })
    };
    let effs: Vec<_> = (0..5).map(|i| step(i, &order)).collect();
    assert!(
      matches!(d.run(fork_all(effs, Concurrency::Sequential)), Exit::Success(v) if v == [0, 1, 2, 3, 4])
    );
    assert_eq!(*order.lock().unwrap(), [0, 1, 2, 3, 4]);

    order.lock().unwrap().clear();
    let effs: Vec<_> = (0..5)
      .map(|i| {
        if i == 2 {
          step(i, &order).then(fail("stop"))
        } else {
          step(i, &order)
        }
      })
      .collect();
    let exit = d.run(fork_all(effs, Concurrency::Sequential));
    assert!(matches!(exit, Exit::Failure(Cause::Fail("stop"))), "{d:?}");
    assert_eq!(
      *order.lock().unwrap(),
      [0, 1, 2],
      "{d:?}: stops at the first failure"
    );
  });
}

#[test]
fn fork_all_bounded_never_exceeds_its_limit() {
  on_both(|d| {
    let (in_flight, high_water) = (counter(), counter());
    let effs: Vec<Effect<usize, Never>> = (0..7)
      .map(|i| {
        let (inc, hw, dec) = (
          Arc::clone(&in_flight),
          Arc::clone(&high_water),
          Arc::clone(&in_flight),
        );
        sync(move || {
          let now = inc.fetch_add(1, Ordering::SeqCst) + 1;
          hw.fetch_max(now, Ordering::SeqCst);
        })
        .then(sleep(Duration::from_millis(10)))
        .then(sync(move || {
          dec.fetch_sub(1, Ordering::SeqCst);
          i
        }))
      })
      .collect();
    let Exit::Success(v) = d.run(fork_all(effs, Concurrency::Bounded(2))) else {
      panic!("{d:?}")
    };
    assert_eq!(v, (0..7).collect::<Vec<_>>(), "{d:?}");
    assert_eq!(
      get(&high_water),
      2,
      "{d:?}: at most (and at some point exactly) 2 in flight"
    );
  });
}

// ── AC9: 10 000 fork + join on tokio, no blocking pool ────────────────────────────────────

/// Delegates to tokio and counts `spawn_blocking` calls.
struct CountingExec {
  inner: TokioExecutor,
  blocking: AtomicUsize,
}

impl Executor for CountingExec {
  fn spawn(&self, fut: BoxFuture<'static, ()>) {
    self.inner.spawn(fut);
  }
  fn spawn_local(&self, fut: LocalBoxFuture<'static, ()>) -> Result<(), NotLocal> {
    self.inner.spawn_local(fut)
  }
  fn spawn_blocking(&self, f: Box<dyn FnOnce() + Send>) {
    self.blocking.fetch_add(1, Ordering::SeqCst);
    self.inner.spawn_blocking(f);
  }
  fn sleep(&self, d: Duration) -> BoxFuture<'static, ()> {
    self.inner.sleep(d)
  }
  fn now(&self) -> Instant {
    self.inner.now()
  }
}

fn fork_join_chain(n: u64) -> Effect<u64, Never> {
  (0..n).fold(succeed(0), |acc, i| {
    acc.flat_map(move |sum| {
      sync(move || i)
        .fork()
        .flat_map(FiberHandle::join)
        .map(move |v| sum + v)
    })
  })
}

#[test]
fn ten_thousand_fork_join_on_tokio_without_the_blocking_pool() {
  let rt = tokio_rt();
  let exec = Arc::new(CountingExec {
    inner: TokioExecutor::new(rt.handle().clone()),
    blocking: AtomicUsize::new(0),
  });
  let n = 10_000;
  let t0 = Instant::now();
  let h = fork_join_chain(n).fork_root(Arc::clone(&exec) as Arc<dyn Executor>);
  let exit = rt.block_on(h.join_future());
  let took = t0.elapsed();
  println!("10 000 fork+join on tokio: {took:?}");
  assert!(matches!(exit, Exit::Success(s) if s == n * (n - 1) / 2));
  assert!(took < Duration::from_secs(2), "took {took:?}");
  assert_eq!(exec.blocking.load(Ordering::SeqCst), 0);
}

#[test]
fn fork_join_chain_on_the_test_executor() {
  assert!(matches!(Driver::Test.run(fork_join_chain(1000)), Exit::Success(s) if s == 999 * 500));
}

// ── handles and ids ───────────────────────────────────────────────────────────────────────

#[test]
fn child_contexts_link_to_their_parent() {
  on_both(|d| {
    let eff = sleep::<Never, _>(Duration::from_millis(1))
      .fork()
      .fork()
      .flat_map(|outer| {
        let octx = Arc::clone(outer.fiber());
        outer.join().flat_map(move |inner: FiberHandle<(), Never>| {
          let parent: Option<FiberId> = inner.fiber().parent().map(|p| p.id());
          inner.join().map(move |()| (parent, octx.id()))
        })
      });
    // The inner child was interrupted when its parent (`outer`) exited, so the join re-raises
    // that interrupt; the parent link was observed while `outer`'s context was still alive.
    match d.run(eff) {
      Exit::Success((parent, outer)) => assert_eq!(parent, Some(outer), "{d:?}"),
      Exit::Failure(Cause::Interrupt(_)) => {}
      other => panic!("{d:?}: {other:?}"),
    }
  });
}
