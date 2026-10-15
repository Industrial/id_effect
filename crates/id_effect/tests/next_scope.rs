//! `k-scope-fiberref` acceptance tests for `id_effect::next`: `Scope` (LIFO finalizers, close
//! once and uninterruptibly), `Effect::scoped`, `Scope::fork_in` and `FiberRef`.
//!
//! Every behavioural test runs on a multi-thread `TokioExecutor` **and** on the deterministic
//! `TestExecutor` through [`on_both`].

use std::future::Future;
use std::pin::pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use id_effect::Never;
use id_effect::next::{
  Effect, Executor, ExitKind, FiberHandle, FiberRef, HNil, Scope, TestExecutor, TokioExecutor, die,
  fail, sleep, succeed, suspend, sync, yield_fiber,
};
use id_effect::{Cause, Exit};

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

type Log = Arc<Mutex<Vec<&'static str>>>;

fn log() -> Log {
  Arc::new(Mutex::new(Vec::new()))
}

fn push<E: Send + 'static>(l: &Log, s: &'static str) -> Effect<(), E> {
  let l = Arc::clone(l);
  sync(move || l.lock().expect("log").push(s))
}

fn entries(l: &Log) -> Vec<&'static str> {
  l.lock().expect("log").clone()
}

fn flag() -> Arc<AtomicBool> {
  Arc::new(AtomicBool::new(false))
}

fn set<E: Send + 'static>(f: &Arc<AtomicBool>) -> Effect<(), E> {
  let f = Arc::clone(f);
  sync(move || f.store(true, Ordering::SeqCst))
}

/// Yields until `f` is set (so another fiber has reached a known point).
fn started<E: Send + 'static>(f: &Arc<AtomicBool>) -> Effect<(), E> {
  fn go<E: Send + 'static>(f: Arc<AtomicBool>) -> Effect<(), E> {
    suspend(move || {
      if f.load(Ordering::SeqCst) {
        succeed(())
      } else {
        yield_fiber().flat_map(move |()| go(f))
      }
    })
  }
  go(Arc::clone(f))
}

fn kind_of<A, E>(exit: &Exit<A, E>) -> ExitKind {
  ExitKind::of(exit)
}

// ── AC1: Scope::new, add_finalizer, close ─────────────────────────────────────────────────

#[test]
fn close_runs_finalizers_in_reverse_order_exactly_once() {
  on_both(|d| {
    let l = log();
    let scope = Scope::new();
    assert!(!scope.is_closed(), "a plain constructor: open, nothing ran");
    let eff: Effect<(), Never> = scope
      .add_finalizer(push(&l, "first"))
      .then(scope.add_finalizer(push(&l, "second")))
      .then(scope.add_finalizer(push(&l, "third")))
      .then(push(&l, "before close"))
      .then(scope.close(ExitKind::Success))
      .then(scope.close(ExitKind::Failure));
    assert!(matches!(d.run(eff), Exit::Success(())), "{d:?}");
    assert_eq!(
      entries(&l),
      ["before close", "third", "second", "first"],
      "{d:?}"
    );
    assert_eq!(
      scope.closed_with(),
      Some(ExitKind::Success),
      "{d:?}: second close is a no-op"
    );
  });
}

#[test]
fn close_feeds_the_exit_kind_and_survives_a_dying_finalizer() {
  on_both(|d| {
    let l = log();
    let kinds = Arc::new(Mutex::new(Vec::new()));
    let scope = Scope::new();
    let k = Arc::clone(&kinds);
    let eff: Effect<(), Never> = scope
      .add_finalizer(push(&l, "a"))
      .then(scope.add_finalizer(die("finalizer died")))
      .then(scope.add_finalizer_exit(move |kind| sync(move || k.lock().expect("kinds").push(kind))))
      .then(scope.close(ExitKind::Interrupt));
    let exit = d.run(eff);
    assert_eq!(kind_of(&exit), ExitKind::Die, "{d:?}: {exit:?}");
    assert_eq!(entries(&l), ["a"], "{d:?}: later finalizers still run");
    assert_eq!(
      *kinds.lock().expect("kinds"),
      [ExitKind::Interrupt],
      "{d:?}"
    );
  });
}

#[test]
fn a_finalizer_added_after_close_runs_at_once() {
  on_both(|d| {
    let l = log();
    let scope = Scope::new();
    let eff: Effect<(), Never> = scope
      .close(ExitKind::Success)
      .then(scope.add_finalizer(push(&l, "late")))
      .then(push(&l, "after"));
    assert!(matches!(d.run(eff), Exit::Success(())), "{d:?}");
    assert_eq!(entries(&l), ["late", "after"], "{d:?}");
  });
}

#[test]
fn close_is_uninterruptible() {
  on_both(|d| {
    let (l, go) = (log(), flag());
    let scope = Scope::new();
    let slow = set(&go)
      .then(sleep(Duration::from_millis(30)))
      .then(push(&l, "slow finalizer done"));
    let closer: Effect<(), Never> = scope
      .add_finalizer(push(&l, "outer"))
      .then(scope.add_finalizer(slow))
      .then(scope.close(ExitKind::Success))
      .then(push(&l, "after close"));
    let eff = closer.fork().flat_map({
      let go = Arc::clone(&go);
      move |h| started(&go).then(h.interrupt())
    });
    let Exit::Success(ex) = d.run(eff) else {
      panic!("{d:?}: root failed")
    };
    assert!(
      matches!(ex, Exit::Failure(Cause::Interrupt(_))),
      "{d:?}: {ex:?}"
    );
    assert_eq!(
      entries(&l),
      ["slow finalizer done", "outer"],
      "{d:?}: interrupt honoured only after close"
    );
  });
}

// ── AC2: Effect::scoped closes on every exit ──────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq)]
enum Tail {
  Succeed,
  Fail,
  Die,
  Interrupt,
}

/// `Effect::scoped` with two finalizers (the second records the closing kind) and `tail`.
fn scoped_with(
  l: &Log,
  kinds: &Arc<Mutex<Vec<ExitKind>>>,
  go: &Arc<AtomicBool>,
  tail: Tail,
) -> Effect<u32, &'static str> {
  let (l, kinds, go) = (Arc::clone(l), Arc::clone(kinds), Arc::clone(go));
  Effect::scoped(move |scope| {
    let k = Arc::clone(&kinds);
    let rest: Effect<u32, &'static str> = match tail {
      Tail::Succeed => succeed(7),
      Tail::Fail => fail("body failed"),
      Tail::Die => sync(|| -> u32 { panic!("body panicked") }),
      Tail::Interrupt => set(&go).then(sleep(HOUR)).then(succeed(7)),
    };
    scope
      .add_finalizer(push(&l, "first"))
      .then(scope.add_finalizer_exit(move |kind| sync(move || k.lock().expect("kinds").push(kind))))
      .then(scope.add_finalizer(push(&l, "second")))
      .then(push(&l, "body"))
      .then(rest)
  })
}

#[test]
fn scoped_closes_on_success_failure_die_and_interrupt() {
  for tail in [Tail::Succeed, Tail::Fail, Tail::Die, Tail::Interrupt] {
    on_both(|d| {
      let (l, kinds, go) = (log(), Arc::new(Mutex::new(Vec::new())), flag());
      let eff = scoped_with(&l, &kinds, &go, tail);
      let exit = if tail == Tail::Interrupt {
        let root = eff.fork().flat_map({
          let go = Arc::clone(&go);
          move |h| started(&go).then(h.interrupt())
        });
        match d.run(root) {
          Exit::Success(ex) => ex,
          other => panic!("{d:?}: root failed: {other:?}"),
        }
      } else {
        d.run(eff)
      };
      let want = match tail {
        Tail::Succeed => ExitKind::Success,
        Tail::Fail => ExitKind::Failure,
        Tail::Die => ExitKind::Die,
        Tail::Interrupt => ExitKind::Interrupt,
      };
      assert_eq!(kind_of(&exit), want, "{d:?}/{tail:?}: {exit:?}");
      if tail == Tail::Succeed {
        assert!(matches!(exit, Exit::Success(7)), "{d:?}");
      }
      assert_eq!(
        entries(&l),
        ["body", "second", "first"],
        "{d:?}/{tail:?}: finalizers in reverse order after the body"
      );
      assert_eq!(*kinds.lock().expect("kinds"), [want], "{d:?}/{tail:?}");
    });
  }
}

#[test]
fn scoped_raises_a_finalizer_defect_after_the_body_outcome() {
  on_both(|d| {
    let eff: Effect<u32, &'static str> = Effect::scoped(|scope| {
      scope
        .add_finalizer(die("release died"))
        .then(fail("body failed"))
    });
    match d.run(eff) {
      Exit::Failure(Cause::Then(a, b)) => {
        assert!(matches!(*a, Cause::Fail("body failed")), "{d:?}");
        assert!(matches!(*b, Cause::Die(_)), "{d:?}");
      }
      other => panic!("{d:?}: expected Then(Fail, Die), got {other:?}"),
    }
  });
}

// ── AC3: fork_in ──────────────────────────────────────────────────────────────────────────

#[test]
fn closing_the_scope_interrupts_a_fork_in_child_while_its_forker_lives() {
  on_both(|d| {
    let (l, go) = (log(), flag());
    let scope = Scope::new();
    let child: Effect<(), Never> = set(&go)
      .then(sleep(HOUR))
      .ensuring(push(&l, "child finalizer"));
    let eff = scope
      .add_finalizer(push(&l, "registered before the fork"))
      .then(scope.fork_in(child))
      .flat_map({
        let (scope, go, l) = (scope.clone(), Arc::clone(&go), Arc::clone(&l));
        move |h: FiberHandle<(), Never>| {
          assert!(h.fiber().parent().is_none(), "owned by the scope");
          started(&go)
            .then(scope.close(ExitKind::Success))
            .then(push(&l, "forker still alive"))
            .then(h.join().exit())
        }
      });
    let Exit::Success(ex) = d.run(eff) else {
      panic!("{d:?}: root failed")
    };
    assert!(
      matches!(ex, Exit::Failure(Cause::Interrupt(_))),
      "{d:?}: {ex:?}"
    );
    assert_eq!(
      entries(&l),
      [
        "child finalizer",
        "registered before the fork",
        "forker still alive"
      ],
      "{d:?}: the child is stopped at its place in the LIFO order"
    );
  });
}

#[test]
fn a_fork_in_child_outlives_its_forking_fiber() {
  on_both(|d| {
    let ran = flag();
    let scope = Scope::new();
    let forker = scope
      .fork_in(sleep(Duration::from_millis(50)).then(set(&ran)))
      .fork();
    let eff = forker.flat_map({
      let (scope, ran) = (scope.clone(), Arc::clone(&ran));
      move |fh| {
        let fctx = Arc::clone(fh.fiber());
        fh.join().flat_map(move |child: FiberHandle<(), Never>| {
          assert!(fctx.is_done(), "forker has exited");
          assert!(!child.is_done(), "fork_in child still running");
          child
            .join()
            .then(scope.close(ExitKind::Success))
            .map(move |()| ran.load(Ordering::SeqCst))
        })
      }
    });
    assert!(matches!(d.run(eff), Exit::Success(true)), "{d:?}");
  });
}

#[test]
fn fork_in_a_closed_scope_yields_an_interrupted_fiber() {
  on_both(|d| {
    let ran = flag();
    let scope = Scope::new();
    let eff = scope
      .close(ExitKind::Success)
      .then(scope.fork_in(set::<Never>(&ran)))
      .flat_map(|h| h.join().exit());
    let Exit::Success(ex) = d.run(eff) else {
      panic!("{d:?}: root failed")
    };
    assert!(
      matches!(ex, Exit::Failure(Cause::Interrupt(_))),
      "{d:?}: {ex:?}"
    );
    assert!(!ran.load(Ordering::SeqCst), "{d:?}: the body never ran");
  });
}

// ── AC4: FiberRef ─────────────────────────────────────────────────────────────────────────

#[test]
fn fiber_ref_get_set_update_and_locally() {
  on_both(|d| {
    let r = FiberRef::new(1u32);
    assert_eq!(*r.initial(), 1);
    let r2 = r.clone();
    let r3 = r.clone();
    let eff: Effect<(u32, u32, u32, u32), Never> = r
      .get()
      .flat_map({
        let r = r.clone();
        move |a| {
          r.set(5)
            .then(r.update(|v| v * 2))
            .then(r.get())
            .map(move |b| (a, b))
        }
      })
      .flat_map(move |(a, b)| r2.locally(42, r2.get()).map(move |c| (a, b, c)))
      .flat_map(move |(a, b, c)| r3.get().map(move |e| (a, b, c, e)));
    assert!(
      matches!(d.run(eff), Exit::Success((1, 10, 42, 10))),
      "{d:?}"
    );
  });
}

#[test]
fn locally_restores_the_value_on_failure_and_interrupt() {
  on_both(|d| {
    let r = FiberRef::new("outer");
    let inner = r.clone();
    let after = r.clone();
    let eff: Effect<(Exit<(), &'static str>, &'static str), &'static str> = r
      .locally("inner", inner.get().flat_map(|_| fail("boom")))
      .exit()
      .flat_map(move |ex| after.get().map(move |v| (ex, v)));
    match d.run(eff) {
      Exit::Success((Exit::Failure(Cause::Fail("boom")), "outer")) => {}
      other => panic!("{d:?}: {other:?}"),
    }

    // Interrupted inside `locally`: the fiber's own finalizer sees the restored value.
    let (go, seen) = (flag(), Arc::new(Mutex::new(None)));
    let r = FiberRef::new(0u8);
    let child: Effect<(), Never> = r.locally(9, set(&go).then(sleep(HOUR))).ensuring({
      let (r, seen) = (r.clone(), Arc::clone(&seen));
      r.get()
        .flat_map(move |v| sync(move || *seen.lock().expect("seen") = Some(v)))
    });
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
    assert_eq!(*seen.lock().expect("seen"), Some(0), "{d:?}");
  });
}

#[test]
fn a_child_starts_with_the_parent_value_at_fork_time_and_its_set_stays_local() {
  on_both(|d| {
    let r = FiberRef::new(0u32);
    let go = flag();
    let child: Effect<(u32, u32), Never> = started(&go).then(r.get()).flat_map({
      let r = r.clone();
      move |at_start| r.set(99).then(r.get()).map(move |own| (at_start, own))
    });
    let eff = r.set(1).then(child.fork()).flat_map({
      let (r, go) = (r.clone(), Arc::clone(&go));
      move |h| {
        r.set(2)
          .then(set(&go))
          .then(h.join())
          .flat_map(move |(seen, own)| r.get().map(move |parent| (seen, own, parent)))
      }
    });
    assert!(
      matches!(d.run(eff), Exit::Success((1, 99, 2))),
      "{d:?}: child saw 1 (fork time), set 99 locally; parent kept 2"
    );
  });
}

#[test]
fn fork_in_and_locally_children_inherit_fiber_refs() {
  on_both(|d| {
    let r = FiberRef::new("init");
    let scope = Scope::new();
    let in_scope = r.locally("local", scope.fork_in(r.get::<Never, HNil>()));
    let eff = in_scope.flat_map({
      let scope = scope.clone();
      move |h| {
        h.join()
          .flat_map(move |v| scope.close(ExitKind::Success).map(move |()| v))
      }
    });
    assert!(matches!(d.run(eff), Exit::Success("local")), "{d:?}");
  });
}

/// Effect.ts parity (`forkDaemon`): a daemon starts with the forker's fiber-ref values at fork
/// time, and its own `set` stays local to it.
#[test]
fn fork_daemon_inherits_fiber_refs_at_fork_time() {
  on_both(|d| {
    let r = FiberRef::new(0u32);
    let daemon: Effect<(u32, u32), Never> = r.get().flat_map({
      let r = r.clone();
      move |at_start| r.set(99).then(r.get()).map(move |own| (at_start, own))
    });
    let eff = r.set(7).then(daemon.fork_daemon()).flat_map({
      let r = r.clone();
      move |h: FiberHandle<(u32, u32), Never>| {
        assert!(h.fiber().parent().is_none(), "daemon has no parent");
        h.join()
          .flat_map(move |(seen, own)| r.get().map(move |parent| (seen, own, parent)))
      }
    });
    assert!(
      matches!(d.run(eff), Exit::Success((7, 99, 7))),
      "{d:?}: daemon saw 7 (fork time), set 99 locally; forker kept 7"
    );
  });
}

// ── AC5: no thread-locals ─────────────────────────────────────────────────────────────────

/// Fiber state (refs included) never lives in thread-locals. `testing/` is skipped: the
/// test-only `TestExecutor` keeps its `spawn_local` task table (executor state, not fiber state)
/// in one, owned by its driving thread. Of `compute/` only the kernel's `Fabric`
/// (`kernel_fabric.rs`) is scanned: the feature-gated Compute Fabric substrate keeps its
/// per-thread adaptive context there (no fiber state).
#[test]
fn kernel_has_no_thread_locals() {
  fn scan(dir: &std::path::Path, hits: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).expect("read_dir") {
      let path = entry.expect("entry").path();
      if path.is_dir() {
        if path.ends_with("testing") {
          continue;
        }
        scan(&path, hits);
      } else if dir.ends_with("compute") && !path.ends_with("kernel_fabric.rs") {
        continue;
      } else if path.extension().is_some_and(|e| e == "rs") {
        let src = std::fs::read_to_string(&path).expect("read");
        if src.contains(concat!("thread", "_local")) {
          hits.push(path.display().to_string());
        }
      }
    }
  }
  let mut hits = Vec::new();
  scan(
    &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
    &mut hits,
  );
  assert!(hits.is_empty(), "thread-locals in the kernel: {hits:?}");
}
