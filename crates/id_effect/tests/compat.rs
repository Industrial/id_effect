//! `c-compat-bridge`: `next::compat::{IntoV1, IntoV1With, IntoNext}` (release plan §3b).
//!
//! Round trips (next → v1 → next and v1 → next → v1) for success, typed failure, `Die` and
//! interrupt; cancelling the v1 side interrupts the next fiber (finalizer flag); interrupting
//! the next side drops the v1 future; the env mappings.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use id_effect::next::compat::{IntoNext, IntoV1, IntoV1With};
use id_effect::next::{self, FiberContext, HCons, HNil, Runtime};
use id_effect::{Cause, Exit, Never, run_blocking};

type V1<A, E = String, R = ()> = id_effect::Effect<A, E, R>;
type Next<A, E = String, R = HNil> = next::Effect<A, E, R>;

/// Waits (up to 5 s of wall time) for `flag`.
fn wait_for(flag: &AtomicBool) -> bool {
  let deadline = Instant::now() + Duration::from_secs(5);
  while !flag.load(Ordering::SeqCst) {
    if Instant::now() > deadline {
      return false;
    }
    std::thread::sleep(Duration::from_millis(2));
  }
  true
}

fn set(flag: &Arc<AtomicBool>) -> Next<(), Never> {
  let flag = Arc::clone(flag);
  next::sync(move || flag.store(true, Ordering::SeqCst))
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> Option<String> {
  payload
    .downcast_ref::<String>()
    .cloned()
    .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_string()))
}

// ── success ──────────────────────────────────────────────────────────────────

#[test]
fn success_next_to_v1() {
  let ready: V1<u32> = next::succeed::<u32, String, HNil>(7).into_v1();
  assert_eq!(run_blocking(ready, ()), Ok(7));
  let forked: V1<u32> = next::sync::<u32, String, HNil>(|| 8).into_v1();
  assert_eq!(run_blocking(forked.map(|n| n * 2), ()), Ok(16));
}

#[test]
fn success_round_trips() {
  let exit = next::sync::<u32, String, HNil>(|| 3)
    .into_v1::<()>()
    .into_next()
    .block_on();
  assert!(matches!(exit, Exit::Success(3)));

  // v1 → next → v1.
  let exit = V1::<u32>::new(|_| Ok(4)).into_next().block_on();
  let Exit::Success(n) = exit else {
    panic!("expected Success, got {exit:?}")
  };
  assert_eq!(
    run_blocking(next::succeed::<u32, String, HNil>(n).into_v1(), ()),
    Ok(4)
  );
}

// ── typed failure ────────────────────────────────────────────────────────────

#[test]
fn typed_failure_next_to_v1_and_back() {
  let v1: V1<u32> = next::sync::<u32, String, HNil>(|| 1)
    .flat_map(|_| next::fail("bad".to_string()))
    .into_v1();
  assert_eq!(run_blocking(v1, ()), Err("bad".to_string()));

  let exit = next::fail::<u32, String, HNil>("bad".into())
    .into_v1::<()>()
    .into_next()
    .block_on();
  assert!(matches!(exit, Exit::Failure(Cause::Fail(ref e)) if e == "bad"));
}

#[test]
fn typed_failure_v1_to_next() {
  let exit = id_effect::fail::<u32, String, ()>("nope".into())
    .into_next()
    .block_on();
  assert!(matches!(exit, Exit::Failure(Cause::Fail(ref e)) if e == "nope"));
}

// ── Die ──────────────────────────────────────────────────────────────────────

#[test]
fn die_becomes_a_v1_panic_and_round_trips() {
  let v1: V1<u32> = next::sync::<u32, String, HNil>(|| panic!("boom")).into_v1();
  let payload = catch_unwind(AssertUnwindSafe(|| run_blocking(v1, ()))).unwrap_err();
  assert_eq!(panic_message(&*payload).as_deref(), Some("boom"));

  let v1: V1<u32> = next::die::<u32, String, HNil>("no payload").into_v1();
  let payload = catch_unwind(AssertUnwindSafe(|| run_blocking(v1, ()))).unwrap_err();
  assert_eq!(panic_message(&*payload).as_deref(), Some("no payload"));

  let exit = next::sync::<u32, String, HNil>(|| panic!("boom"))
    .into_v1::<()>()
    .into_next()
    .block_on();
  match exit {
    Exit::Failure(Cause::Die(d)) => assert_eq!(d.message(), "boom"),
    other => panic!("expected Die, got {other:?}"),
  }
}

#[derive(Debug, PartialEq)]
struct Payload(u8);

#[test]
fn a_v1_panic_dies_in_next_with_its_payload_and_round_trips() {
  let v1: V1<u32> = V1::new(|_| std::panic::resume_unwind(Box::new(Payload(3))));
  match v1.into_next().block_on() {
    Exit::Failure(Cause::Die(d)) => assert_eq!(d.with_payload(|p: &Payload| p.0), Some(3)),
    other => panic!("expected Die, got {other:?}"),
  }

  // v1 → next → v1: the payload survives both hops.
  let v1: V1<u32> = V1::new(|_| std::panic::resume_unwind(Box::new(Payload(5))));
  let Exit::Failure(Cause::Die(d)) = v1.into_next().block_on() else {
    panic!("expected Die")
  };
  let back: V1<u32> = next::die::<u32, String, HNil>(d).into_v1();
  let payload = catch_unwind(AssertUnwindSafe(|| run_blocking(back, ()))).unwrap_err();
  assert_eq!(payload.downcast_ref::<Payload>(), Some(&Payload(5)));
}

#[test]
fn lossless_views_keep_the_cause() {
  let exit = run_blocking(
    next::die::<u32, String, HNil>("x").into_v1_exit::<(), Never>(),
    (),
  );
  assert!(matches!(exit, Ok(Exit::Failure(Cause::Die(ref d))) if d.message() == "x"));

  let r = run_blocking(
    next::fail::<u32, String, HNil>("e".into()).into_v1_cause::<()>(),
    (),
  );
  assert!(matches!(r, Err(Cause::Fail(ref e)) if e == "e"));
}

// ── interrupt ────────────────────────────────────────────────────────────────

#[test]
fn cancelling_the_v1_side_interrupts_the_next_fiber() {
  let started = Arc::new(AtomicBool::new(false));
  let finalized = Arc::new(AtomicBool::new(false));
  let body: Next<(), String> = set(&started)
    .widen()
    .then(next::sleep(Duration::from_secs(60)))
    .ensuring(set(&finalized));
  let v1: V1<()> = body.into_v1();

  let mut env = ();
  {
    let mut fut = pin!(v1.run(&mut env));
    let mut cx = Context::from_waker(Waker::noop());
    assert!(fut.as_mut().poll(&mut cx).is_pending());
    assert!(wait_for(&started), "the next fiber never started");
    assert!(fut.as_mut().poll(&mut cx).is_pending());
    assert!(!finalized.load(Ordering::SeqCst));
    // v1 cancellation: drop the future.
  }
  assert!(
    wait_for(&finalized),
    "dropping the v1 future must interrupt the next fiber and run its finalizer"
  );
}

#[test]
fn next_interrupt_round_trips_through_v1() {
  let handle = Runtime::global().fork(next::sleep::<String, HNil>(Duration::from_secs(60)));
  let id = handle.id();
  handle.interrupt_now();
  let exit = handle.join::<HNil>().into_v1::<()>().into_next().block_on();
  assert!(matches!(exit, Exit::Failure(Cause::Interrupt(i)) if i == id));
}

struct DropFlag(Arc<AtomicBool>);

impl Drop for DropFlag {
  fn drop(&mut self) {
    self.0.store(true, Ordering::SeqCst);
  }
}

#[test]
fn interrupting_the_next_side_drops_the_v1_future() {
  let dropped = Arc::new(AtomicBool::new(false));
  let flag = Arc::clone(&dropped);
  let v1: V1<()> = V1::new_async(move |_| {
    Box::pin(async move {
      let _guard = DropFlag(flag);
      std::future::pending::<Result<(), String>>().await
    })
  });
  let fiber = FiberContext::new_root(Arc::clone(Runtime::global().executor()));
  let mut run = pin!(v1.into_next().run_on(Arc::clone(&fiber)));
  let mut cx = Context::from_waker(Waker::noop());
  assert!(run.as_mut().poll(&mut cx).is_pending());
  assert!(!dropped.load(Ordering::SeqCst));
  fiber.request_interrupt();
  match run.as_mut().poll(&mut cx) {
    Poll::Ready(Exit::Failure(Cause::Interrupt(id))) => assert_eq!(id, fiber.id()),
    other => panic!("expected Interrupt, got {other:?}"),
  }
  assert!(dropped.load(Ordering::SeqCst));
}

// ── env mapping ──────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
struct AppEnv {
  base: u32,
}

struct Base(u32);

#[test]
fn into_v1_with_builds_the_next_env_from_the_v1_env() {
  let eff: Next<u32, String, HCons<Base, HNil>> = next::need::<Base>().map(|b| b.0 + 1);
  let v1: V1<u32, String, AppEnv> =
    eff.into_v1_with(|env: &mut AppEnv| HCons::new(Base(env.base), HNil));
  assert_eq!(run_blocking(v1, AppEnv { base: 41 }), Ok(42));
}

#[test]
fn into_next_with_and_from_supply_the_v1_env() {
  let read = || V1::<u32, String, AppEnv>::new(|env: &mut AppEnv| Ok(env.base));
  assert!(matches!(
    read().into_next_with(AppEnv { base: 9 }).block_on(),
    Exit::Success(9)
  ));

  let from: next::LocalEffect<u32, String, HCons<AppEnv, HNil>> = read().into_next_from();
  assert!(matches!(
    from.provide(AppEnv { base: 10 }).block_on(),
    Exit::Success(10)
  ));

  let under: next::LocalEffect<u32, String, HCons<Base, HNil>> =
    id_effect::succeed::<u32, String, ()>(11).into_next_in();
  assert!(matches!(
    under.provide(Base(0)).block_on(),
    Exit::Success(11)
  ));
}
