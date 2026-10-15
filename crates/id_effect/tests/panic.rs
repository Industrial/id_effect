//! `c-panic-boundary` acceptance tests for `id_effect`: a panic anywhere in user code
//! becomes `Cause::Die` with its payload kept, and finalizers still run.
//!
//! Every case runs on both run edges ([`Edge::BlockOn`]: `Effect::block_on` on the global
//! runtime; [`Edge::Run`]: `Runtime::run(..)` awaited on a tokio runtime), and covers the
//! places a panic can start: a sync step, a `map` closure on the fused fast path, a future's
//! first poll, a suspended future's later poll, a step after a suspension, a forked fiber, a
//! finalizer (`ensuring`, `acquire_use_release`) and the `LocalEffect` runloop.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::task::{Context, Poll};
use std::time::Duration;

use id_effect::Never;
use id_effect::{Cause, Exit};
use id_effect::{
  Effect, ExitKind, FiberContext, FiberHandle, LocalEffect, Runtime, TokioExecutor,
  acquire_use_release, fail, from_async, sleep, succeed, sync,
};

// ── drivers ───────────────────────────────────────────────────────────────────────────────

fn tokio_rt() -> &'static tokio::runtime::Runtime {
  static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
  RT.get_or_init(|| {
    tokio::runtime::Builder::new_multi_thread()
      .worker_threads(2)
      .enable_time()
      .build()
      .expect("tokio runtime")
  })
}

#[derive(Clone, Copy, Debug)]
enum Edge {
  /// `Effect::block_on` (the global runtime, the calling thread parked between wake-ups).
  BlockOn,
  /// `Runtime::run(eff)` awaited inside a tokio runtime.
  Run,
}

const EDGES: [Edge; 2] = [Edge::BlockOn, Edge::Run];

impl Edge {
  fn run<A: Send + 'static, E: Send + 'static>(self, eff: Effect<A, E>) -> Exit<A, E> {
    match self {
      Edge::BlockOn => eff.block_on(),
      Edge::Run => {
        let rt = tokio_rt();
        let runtime = Runtime::from_executor(Arc::new(TokioExecutor::new(rt.handle().clone())));
        rt.block_on(async move {
          tokio::time::timeout(Duration::from_secs(30), runtime.run(eff))
            .await
            .expect("Runtime::run timed out")
        })
      }
    }
  }
}

// ── panicking building blocks ─────────────────────────────────────────────────────────────

/// A custom (non-string) panic payload.
#[derive(Debug, PartialEq, Eq)]
struct Payload(u32);

/// A sync step that panics with the `&str` payload `"boom"`.
fn boom<A: Send + 'static, E: Send + 'static>() -> Effect<A, E> {
  sync(|| std::panic::panic_any("boom"))
}

/// A sync step that panics with `Payload(42)`.
fn custom<A: Send + 'static, E: Send + 'static>() -> Effect<A, E> {
  sync(|| std::panic::panic_any(Payload(42)))
}

/// A finalizer that panics with the `String` payload `"fin"`.
fn fin_boom() -> Effect<(), Never> {
  sync(|| panic!("{}", "fin"))
}

/// A future that is `Pending` on its first poll (waking itself) and panics on the second.
struct PanicOnSecondPoll(bool);

impl Future for PanicOnSecondPoll {
  type Output = Result<u32, &'static str>;

  fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
    if self.0 {
      std::panic::panic_any("boom");
    }
    self.0 = true;
    cx.waker().wake_by_ref();
    Poll::Pending
  }
}

/// A future that panics on its first poll.
fn panic_on_first_poll() -> Effect<u32, &'static str> {
  from_async(|| {
    std::future::poll_fn(|_| -> Poll<Result<u32, &'static str>> { std::panic::panic_any("boom") })
  })
}

// ── assertions ────────────────────────────────────────────────────────────────────────────

#[track_caller]
fn assert_die_boom<E: std::fmt::Debug>(cause: &Cause<E>, ctx: &str) {
  match cause {
    Cause::Die(d) => {
      assert_eq!(d.message(), "boom", "{ctx}");
      assert!(d.has_payload(), "{ctx}: the panic payload is kept");
    }
    other => panic!("{ctx}: expected Die(\"boom\"), got {other:?}"),
  }
}

#[track_caller]
fn assert_exit_boom<A: std::fmt::Debug, E: std::fmt::Debug>(exit: &Exit<A, E>, ctx: &str) {
  match exit {
    Exit::Failure(c) => assert_die_boom(c, ctx),
    other => panic!("{ctx}: expected Failure(Die), got {other:?}"),
  }
}

#[track_caller]
fn assert_exit_custom<A: std::fmt::Debug, E: std::fmt::Debug>(exit: &Exit<A, E>, ctx: &str) {
  let Exit::Failure(Cause::Die(d)) = exit else {
    panic!("{ctx}: expected Failure(Die), got {exit:?}")
  };
  assert_eq!(d.message(), "non-string panic payload", "{ctx}");
  assert_eq!(
    d.with_payload(|p: &Payload| p.0),
    Some(42),
    "{ctx}: borrowed"
  );
  let taken = d.take_payload().expect("payload present");
  assert_eq!(
    *taken.downcast::<Payload>().expect("downcasts"),
    Payload(42),
    "{ctx}"
  );
  assert!(!d.has_payload(), "{ctx}: taken once");
}

/// `Then(_, Die("fin"))`; returns the left cause.
#[track_caller]
fn assert_then_fin<'c, E: std::fmt::Debug>(exit: &'c Exit<u32, E>, ctx: &str) -> &'c Cause<E> {
  match exit {
    Exit::Failure(Cause::Then(l, r)) => {
      match &**r {
        Cause::Die(d) => assert_eq!(d.message(), "fin", "{ctx}"),
        other => panic!("{ctx}: expected Then(_, Die(\"fin\")), right is {other:?}"),
      }
      l
    }
    other => panic!("{ctx}: expected Failure(Then(_, Die)), got {other:?}"),
  }
}

fn counter() -> Arc<AtomicUsize> {
  Arc::new(AtomicUsize::new(0))
}

fn bump(c: &Arc<AtomicUsize>) -> Effect<(), Never> {
  let c = Arc::clone(c);
  sync(move || {
    c.fetch_add(1, Ordering::SeqCst);
  })
}

// ── AC1: a panicking fiber dies with the payload ──────────────────────────────────────────

#[test]
fn a_panicking_step_dies_with_message_boom() {
  for edge in EDGES {
    let ctx = format!("{edge:?}/sync");
    assert_exit_boom(&edge.run(boom::<u32, &str>()), &ctx);

    let ctx = format!("{edge:?}/map on the fused path");
    let eff: Effect<u32, &str> = succeed(1).map(|_: u32| -> u32 { std::panic::panic_any("boom") });
    assert_exit_boom(&edge.run(eff), &ctx);

    let ctx = format!("{edge:?}/after a suspension");
    let eff: Effect<u32, &str> = sleep(Duration::from_millis(1)).then(boom());
    assert_exit_boom(&edge.run(eff), &ctx);
  }
}

#[test]
fn a_panicking_future_poll_dies_with_message_boom() {
  for edge in EDGES {
    let ctx = format!("{edge:?}/first poll");
    assert_exit_boom(&edge.run(panic_on_first_poll()), &ctx);

    let ctx = format!("{edge:?}/suspended future, second poll");
    let eff: Effect<u32, &str> = from_async(|| PanicOnSecondPoll(false));
    assert_exit_boom(&edge.run(eff), &ctx);
  }
}

#[test]
fn a_custom_panic_payload_downcasts() {
  for edge in EDGES {
    assert_exit_custom(&edge.run(custom::<u32, &str>()), &format!("{edge:?}/root"));

    let eff: Effect<u32, &str> = custom::<u32, &str>()
      .fork()
      .widen()
      .flat_map(FiberHandle::join);
    assert_exit_custom(&edge.run(eff), &format!("{edge:?}/forked"));
  }
}

#[test]
fn a_panicking_forked_fiber_dies_and_the_parent_carries_on() {
  for edge in EDGES {
    // Joined: the defect is re-raised in the parent.
    let eff: Effect<u32, &str> = boom::<u32, &str>()
      .fork()
      .widen()
      .flat_map(FiberHandle::join);
    assert_exit_boom(&edge.run(eff), &format!("{edge:?}/sync child joined"));

    let eff: Effect<u32, &str> = from_async(|| PanicOnSecondPoll(false))
      .fork()
      .widen()
      .flat_map(FiberHandle::join);
    assert_exit_boom(&edge.run(eff), &format!("{edge:?}/async child joined"));

    // Observed as a value: the panic stays inside the child fiber.
    let eff: Effect<(bool, u32), &str> = boom::<u32, &str>()
      .fork()
      .widen()
      .flat_map(|h| h.join::<_>().exit())
      .map(|child: Exit<u32, &str>| {
        assert_exit_boom(&child, "child exit");
        (true, 7)
      });
    assert!(
      matches!(edge.run(eff), Exit::Success((true, 7))),
      "{edge:?}: parent succeeds after its child panicked"
    );
  }
}

// ── AC2: a panicking finalizer sequences a defect after the outcome ───────────────────────

#[test]
fn a_panicking_finalizer_produces_then_die() {
  for edge in EDGES {
    // Typed failure, then the finalizer's defect.
    let eff: Effect<u32, &str> = fail("e").ensuring(fin_boom());
    let ex = edge.run(eff);
    let left = assert_then_fin(&ex, &format!("{edge:?}/fail"));
    assert!(matches!(left, Cause::Fail("e")), "{edge:?}: {left:?}");

    // A panicking body, then a panicking finalizer: both defects, in order.
    let eff: Effect<u32, &str> = boom::<u32, &str>().ensuring(fin_boom());
    let ex = edge.run(eff);
    let left = assert_then_fin(&ex, &format!("{edge:?}/die"));
    assert_die_boom(left, &format!("{edge:?}/die left"));

    // Async body and async finalizer.
    let eff: Effect<u32, &str> = sleep(Duration::from_millis(1))
      .then(fail("e"))
      .ensuring(sleep(Duration::from_millis(1)).then(fin_boom()));
    let ex = edge.run(eff);
    let left = assert_then_fin(&ex, &format!("{edge:?}/async"));
    assert!(matches!(left, Cause::Fail("e")), "{edge:?}: {left:?}");

    // After a success, the finalizer's defect replaces it (no `Then` with nothing on the left).
    let eff: Effect<u32, &str> = succeed(1).ensuring(fin_boom());
    match edge.run(eff) {
      Exit::Failure(Cause::Die(d)) => assert_eq!(d.message(), "fin", "{edge:?}/success"),
      other => panic!("{edge:?}/success: expected Die(\"fin\"), got {other:?}"),
    }

    // In a forked fiber.
    let eff: Effect<u32, &str> = fail("e")
      .ensuring(fin_boom())
      .fork()
      .widen()
      .flat_map(FiberHandle::join);
    let ex = edge.run(eff);
    let left = assert_then_fin(&ex, &format!("{edge:?}/forked"));
    assert!(matches!(left, Cause::Fail("e")), "{edge:?}: {left:?}");
  }
}

#[test]
fn finalizers_run_after_a_panic_and_after_a_panicking_finalizer() {
  for edge in EDGES {
    let (inner, outer) = (counter(), counter());
    let eff: Effect<u32, &str> = boom::<u32, &str>()
      .ensuring(fin_boom())
      .ensuring(bump(&inner))
      .ensuring(bump(&outer));
    let ex = edge.run(eff);
    let left = assert_then_fin(&ex, &format!("{edge:?}"));
    assert_die_boom(left, &format!("{edge:?}/left"));
    assert_eq!(
      inner.load(Ordering::SeqCst),
      1,
      "{edge:?}: inner finalizer ran"
    );
    assert_eq!(
      outer.load(Ordering::SeqCst),
      1,
      "{edge:?}: outer finalizer ran"
    );
  }
}

#[test]
fn a_panicking_release_produces_then_die() {
  for edge in EDGES {
    let kinds = Arc::new(std::sync::Mutex::new(Vec::new()));
    let eff: Effect<u32, &str> = acquire_use_release(succeed(5u32), |_| boom::<u32, &str>(), {
      let kinds = Arc::clone(&kinds);
      move |_, kind| {
        kinds.lock().expect("kinds").push(kind);
        fin_boom()
      }
    });
    let ex = edge.run(eff);
    let left = assert_then_fin(&ex, &format!("{edge:?}"));
    assert_die_boom(left, &format!("{edge:?}/left"));
    assert_eq!(
      *kinds.lock().expect("kinds"),
      vec![ExitKind::Die],
      "{edge:?}: release ran once and saw the use panic as Die"
    );
  }
}

// ── LocalEffect runloop ───────────────────────────────────────────────────────────────────

/// Runs a `LocalEffect` on both edges: `LocalEffect::block_on`, and its root-fiber future
/// (`run_fiber`, the `!Send` twin of `Runtime::run`) driven on the calling thread.
fn run_local(
  eff: impl Fn() -> LocalEffect<u32, &'static str>,
  check: impl Fn(&Exit<u32, &str>, &str),
) {
  check(&eff().block_on(), "LocalEffect::block_on");
  let exec = Arc::clone(Runtime::global().executor());
  let exit = Runtime::global()
    .block_on_future(eff().run_fiber(FiberContext::new_root(exec)))
    .expect("not in an async context");
  check(&exit, "LocalEffect::run_fiber");
}

#[test]
fn a_panicking_local_effect_dies_with_the_payload() {
  run_local(
    || LocalEffect::sync(|| std::panic::panic_any("boom")),
    |ex, ctx| assert_exit_boom(ex, ctx),
  );
  run_local(
    || LocalEffect::sync(|| std::panic::panic_any(Payload(42))),
    |ex, ctx| assert_exit_custom(ex, ctx),
  );
  run_local(
    || LocalEffect::from_future(|| PanicOnSecondPoll(false)),
    |ex, ctx| assert_exit_boom(ex, ctx),
  );
}

#[test]
fn a_panicking_local_finalizer_produces_then_die() {
  run_local(
    || LocalEffect::fail("e").ensuring(LocalEffect::sync(|| panic!("{}", "fin"))),
    |ex, ctx| {
      let left = assert_then_fin(ex, ctx);
      assert!(matches!(left, Cause::Fail("e")), "{ctx}: {left:?}");
    },
  );
  run_local(
    || {
      LocalEffect::sync(|| std::panic::panic_any("boom"))
        .ensuring(LocalEffect::sync(|| panic!("{}", "fin")))
    },
    |ex, ctx| assert_die_boom(assert_then_fin(ex, ctx), ctx),
  );
}
