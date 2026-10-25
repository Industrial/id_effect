//! `r13-f-any-env` (F52 / D14) acceptance tests: `Effect::any_env` and `LocalEffect::any_env`
//! run an environment-free effect under any environment `R` (the unit env, an HList, a bare
//! `DynEnv`) without changing its outcome, its interruption or its finalizers.

use std::future::Future;
use std::pin::pin;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use id_effect::{
  Cause, Ctx, DynEnv, Effect, Executor, Exit, HCons, HNil, LocalEffect, Never, TestExecutor, die,
  fail, from_async, sleep, succeed, suspend, sync, try_need, yield_fiber,
};

const HOUR: Duration = Duration::from_secs(3600);

/// Polls `fut` once; it must be ready (the test executor has already run everything).
fn take_ready<F: Future>(fut: F) -> F::Output {
  let mut fut = pin!(fut);
  match fut.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
    Poll::Ready(v) => v,
    Poll::Pending => panic!("root fiber is not done"),
  }
}

/// Runs `eff` as a root fiber on a deterministic test executor, advancing virtual time while it
/// waits on a timer.
fn run<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> Exit<A, E> {
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

/// The three shapes of an env-free effect: ready, fused synchronous step, erased program.
fn shapes() -> Vec<(&'static str, Effect<u32, String>)> {
  vec![
    ("ready", succeed(7)),
    ("sync", sync(|| 7)),
    ("fused", succeed::<u32, String, HNil>(3).map(|n| n + 4)),
    ("program", from_async(|| async { Ok(7) })),
    (
      "sync-to-program",
      suspend(|| from_async(|| async { Ok(7) })),
    ),
    (
      "async-chain",
      yield_fiber::<String, HNil>().then(from_async(|| async { Ok(7) })),
    ),
  ]
}

/// An effect over a bare `DynEnv` that reads the environment it runs under.
fn dyn_len() -> Effect<usize, String, DynEnv> {
  Effect::from_ctx(|ctx: Ctx<DynEnv>| {
    let n = ctx.env().len();
    async move { Ok(n) }
  })
}

struct Marker(&'static str);

#[test]
fn any_env_runs_under_unit_and_dyn_env() {
  for (name, eff) in shapes() {
    // Under the unit environment (`HNil` itself and the literal `()`).
    let unit: Effect<u32, String, HNil> = eff.any_env();
    assert!(matches!(run(unit), Exit::Success(7)), "{name}: HNil");
  }
  for (name, eff) in shapes() {
    let unit: Effect<u32, String, ()> = eff.any_env();
    assert!(
      matches!(run(unit.provide_all(())), Exit::Success(7)),
      "{name}: ()"
    );
  }
  for (name, eff) in shapes() {
    // Under a bare `DynEnv`: the environment outside the lifted effect is untouched, before and
    // after it ran.
    let procedure: Effect<(usize, u32, usize), String, DynEnv> = dyn_len()
      .flat_map(move |before| eff.any_env().map(move |v| (before, v)))
      .flat_map(|(before, v)| dyn_len().map(move |after| (before, v, after)));
    let env = DynEnv::new().with(Marker("audit"));
    assert!(
      matches!(run(procedure.provide_all(env)), Exit::Success((1, 7, 1))),
      "{name}: DynEnv"
    );
  }
  for (name, eff) in shapes() {
    // Under an HList holding a `DynEnv`, next to a capability lookup.
    let procedure: Effect<(u32, &'static str), String, HCons<DynEnv, HNil>> =
      eff.any_env().flat_map(|v| {
        try_need::<Marker>()
          .map(move |m| (v, m.0))
          .map_err(|e| e.to_string())
      });
    let env = HCons::new(DynEnv::new().with(Marker("audit")), HNil);
    assert!(
      matches!(run(procedure.provide_all(env)), Exit::Success((7, "audit"))),
      "{name}: HCons<DynEnv, HNil>"
    );
  }

  // Typed failures and defects pass through unchanged, ready or not.
  let failed: Effect<u32, String, DynEnv> = fail::<u32, String, HNil>("no".into()).any_env();
  assert!(matches!(
    run(failed.provide_all(DynEnv::new())),
    Exit::Failure(Cause::Fail(e)) if e == "no"
  ));
  let failed_later: Effect<u32, String, DynEnv> =
    from_async::<u32, String, HNil, _, _>(|| async { Err("later".to_owned()) }).any_env();
  assert!(matches!(
    run(failed_later.provide_all(DynEnv::new())),
    Exit::Failure(Cause::Fail(e)) if e == "later"
  ));
  let died: Effect<u32, String, DynEnv> = sync::<u32, String, HNil>(|| panic!("boom")).any_env();
  assert!(matches!(
    run(died.provide_all(DynEnv::new())),
    Exit::Failure(Cause::Die(_))
  ));
  let dead: Effect<u32, String, DynEnv> = die::<u32, String, HNil>("dead").any_env();
  assert!(matches!(
    run(dead.provide_all(DynEnv::new())),
    Exit::Failure(Cause::Die(_))
  ));
}

#[test]
fn any_env_preserves_interruption_and_finalizers() {
  let go = Arc::new(AtomicBool::new(false));
  let fin = Arc::new(AtomicUsize::new(0));
  let outer_fin = Arc::new(AtomicUsize::new(0));

  // An env-free child that marks `go`, sleeps for an hour and bumps `fin` in its finalizer.
  let child: Effect<(), Never> = {
    let (go, fin) = (Arc::clone(&go), Arc::clone(&fin));
    sync(move || go.store(true, Ordering::SeqCst))
      .then(sleep(HOUR))
      .ensuring(sync(move || {
        fin.fetch_add(1, Ordering::SeqCst);
      }))
  };
  // Lifted into a `DynEnv` procedure with a finalizer of its own that reads the environment.
  let procedure: Effect<(), Never, DynEnv> = {
    let outer_fin = Arc::clone(&outer_fin);
    child
      .any_env()
      .ensuring(Effect::from_ctx(move |ctx: Ctx<DynEnv>| {
        outer_fin.fetch_add(ctx.env().len(), Ordering::SeqCst);
        async { Ok(()) }
      }))
  };
  let env = DynEnv::new().with(Marker("audit"));

  let root: Effect<Exit<(), Never>> = procedure.provide_all(env).fork().flat_map({
    let go = Arc::clone(&go);
    move |h| wait_until(move || go.load(Ordering::SeqCst)).then(h.interrupt())
  });
  let Exit::Success(child_exit) = run(root) else {
    panic!("root failed")
  };
  assert!(
    matches!(child_exit, Exit::Failure(Cause::Interrupt(_))),
    "{child_exit:?}"
  );
  assert_eq!(fin.load(Ordering::SeqCst), 1, "inner finalizer ran once");
  assert_eq!(
    outer_fin.load(Ordering::SeqCst),
    1,
    "outer finalizer ran under the restored DynEnv"
  );

  // An uninterruptible region inside the lifted effect still defers the interrupt to its end.
  let done = Arc::new(AtomicBool::new(false));
  let masked: Effect<(), Never> = {
    let (go, done) = (Arc::new(AtomicBool::new(false)), Arc::clone(&done));
    let started = Arc::clone(&go);
    let lifted: Effect<(), Never, DynEnv> = sync::<(), Never, HNil>(move || {
      started.store(true, Ordering::SeqCst);
    })
    .then(sleep(Duration::from_secs(1)))
    .then(sync(move || done.store(true, Ordering::SeqCst)))
    .uninterruptible()
    .any_env();
    lifted.provide_all(DynEnv::new()).fork().flat_map(move |h| {
      wait_until(move || go.load(Ordering::SeqCst))
        .then(h.interrupt())
        .map(|_| ())
    })
  };
  assert!(matches!(run(masked), Exit::Success(())));
  assert!(
    done.load(Ordering::SeqCst),
    "the masked region ran to completion"
  );
}

/// Yields until `pred` holds (so a forked child has reached a known point).
fn wait_until(pred: impl Fn() -> bool + Send + Sync + 'static) -> Effect<(), Never> {
  fn go(pred: Arc<dyn Fn() -> bool + Send + Sync>) -> Effect<(), Never> {
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

#[test]
fn local_any_env_round_trip() {
  // Ready: no program to run under the new environment.
  let ready: LocalEffect<u8, String, DynEnv> = LocalEffect::<u8, String>::succeed(3).any_env();
  assert!(matches!(
    ready.provide_env(DynEnv::new()).block_on(),
    Exit::Success(3)
  ));
  let ready_fail: LocalEffect<u8, String, ()> =
    LocalEffect::<u8, String>::fail("no".into()).any_env();
  assert!(matches!(
    ready_fail.provide_env(()).block_on(),
    Exit::Failure(Cause::Fail(e)) if e == "no"
  ));

  // A `!Send` program lifted into a `DynEnv`, between two reads of that environment, and back
  // down to `HNil` with `provide_env`.
  let shared = Rc::new(5_u8);
  let s = Rc::clone(&shared);
  let lifted: LocalEffect<u8, String, DynEnv> =
    LocalEffect::<u8, String>::sync(move || *s * 2).any_env();
  let len = || {
    LocalEffect::<usize, String, DynEnv>::from_ctx(|ctx| {
      let n = ctx.env().len();
      async move { Ok(n) }
    })
  };
  let procedure = len()
    .flat_map(move |before| lifted.map(move |v| (before, v)))
    .flat_map(move |(before, v)| len().map(move |after| (before, v, after)));
  let back: LocalEffect<(usize, u8, usize), String> =
    procedure.provide_env(DynEnv::new().with(Marker("a")).with(5_u32));
  assert!(matches!(back.block_on(), Exit::Success((2, 10, 2))));
  assert_eq!(
    Rc::strong_count(&shared),
    1,
    "the `!Send` capture was dropped"
  );

  // A lifted program that fails or dies keeps its cause.
  let died: LocalEffect<u8, String, DynEnv> =
    LocalEffect::<u8, String>::sync(|| panic!("boom")).any_env();
  assert!(matches!(
    died.provide_env(DynEnv::new()).block_on(),
    Exit::Failure(Cause::Die(_))
  ));

  // A `Send` effect turned local, then lifted.
  let from_send: LocalEffect<u32, String, DynEnv> = succeed::<u32, String, HNil>(4)
    .map(|n| n + 1)
    .local()
    .any_env();
  assert!(matches!(
    from_send.provide_env(DynEnv::new()).block_on(),
    Exit::Success(5)
  ));
}
