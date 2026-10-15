//! `k-core-runloop` acceptance tests for the `id_effect::next` kernel: inline `Pure`, `Send`,
//! laziness, stack safety, interrupt points, panics as `Die`, and the core combinators.
//!
//! Effects are driven with `pollster::block_on` over a test-local `Executor` stub, because the
//! run edges land in `k-edge`.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use id_effect::next::{
  BoxFuture, Ctx, Effect, Executor, ExitKind, FiberContext, HCons, HNil, LocalBoxFuture, NotLocal,
  die, fail, from_async, sleep, succeed, suspend, sync, yield_fiber,
};
use id_effect::{Cause, Exit};
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};

#[global_allocator]
static GLOBAL: &StatsAlloc<std::alloc::System> = &INSTRUMENTED_SYSTEM;

/// Spawns nothing; `sleep` completes at once and records the requested duration.
#[derive(Default)]
struct StubExec {
  slept: Mutex<Vec<Duration>>,
}

impl Executor for StubExec {
  fn spawn(&self, _: BoxFuture<'static, ()>) {
    unreachable!("next_kernel tests spawn no tasks")
  }
  fn spawn_local(&self, _: LocalBoxFuture<'static, ()>) -> Result<(), NotLocal> {
    Err(NotLocal)
  }
  fn spawn_blocking(&self, _: Box<dyn FnOnce() + Send>) {
    unreachable!("next_kernel tests spawn no blocking work")
  }
  fn sleep(&self, d: Duration) -> BoxFuture<'static, ()> {
    self.slept.lock().unwrap().push(d);
    Box::pin(async {})
  }
  fn now(&self) -> Instant {
    Instant::now()
  }
}

fn fiber() -> Arc<FiberContext> {
  FiberContext::new_root(Arc::new(StubExec::default()))
}

fn run<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> Exit<A, E> {
  pollster::block_on(eff.run_on(fiber()))
}

fn counter() -> Arc<AtomicUsize> {
  Arc::new(AtomicUsize::new(0))
}

fn bump(c: &Arc<AtomicUsize>) {
  c.fetch_add(1, Ordering::SeqCst);
}

fn get(c: &Arc<AtomicUsize>) -> usize {
  c.load(Ordering::SeqCst)
}

// ── AC1: inline Pure, zero allocations ─────────────────────────────────────────────────────

#[test]
fn succeed_and_drive_through_the_pure_fast_path_allocates_nothing() {
  let fiber = fiber();
  let waker = Waker::noop();
  let mut cx = Context::from_waker(waker);
  let region = Region::new(GLOBAL);
  let eff: Effect<u32> = succeed(1);
  let mut fut = eff.run_on(Arc::clone(&fiber));
  let out = Pin::new(&mut fut).poll(&mut cx);
  let stats = region.change();
  assert!(matches!(out, Poll::Ready(Exit::Success(1))));
  assert_eq!(stats.allocations, 0, "{stats:?}");
  assert_eq!(stats.reallocations, 0, "{stats:?}");
}

// ── AC2: Send ──────────────────────────────────────────────────────────────────────────────

fn assert_send<T: Send>() {}

#[test]
fn effects_are_send() {
  type Caps = HCons<u8, HNil>;
  assert_send::<Effect<u8, (), Caps>>();
  assert_send::<Effect<u8, (), HNil>>();
  assert_send::<Effect<u8>>();
}

// ── AC3: laziness ──────────────────────────────────────────────────────────────────────────

#[test]
fn combinators_run_nothing_before_the_effect_is_driven() {
  let (f_runs, g_runs) = (counter(), counter());
  let (f2, g2) = (Arc::clone(&f_runs), Arc::clone(&g_runs));
  let eff: Effect<u32> = succeed(1)
    .flat_map(move |x| {
      bump(&f2);
      succeed(x + 1)
    })
    .map(move |x| {
      bump(&g2);
      x * 10
    });
  assert_eq!((get(&f_runs), get(&g_runs)), (0, 0));
  assert_eq!(run(eff), Exit::succeed(20));
  assert_eq!((get(&f_runs), get(&g_runs)), (1, 1));
}

#[test]
fn sync_and_suspend_are_lazy() {
  let c = counter();
  let (c1, c2) = (Arc::clone(&c), Arc::clone(&c));
  let a: Effect<u32> = sync(move || {
    bump(&c1);
    7
  });
  let b: Effect<u32> = suspend(move || {
    bump(&c2);
    succeed(8)
  });
  assert_eq!(get(&c), 0);
  assert_eq!(run(a.zip(b)), Exit::succeed((7, 8)));
  assert_eq!(get(&c), 2);
}

// ── AC4: stack safety ──────────────────────────────────────────────────────────────────────

fn depth() -> u64 {
  if cfg!(debug_assertions) {
    10_000
  } else {
    100_000
  }
}

fn on_small_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
  std::thread::Builder::new()
    .stack_size(2 << 20)
    .spawn(f)
    .expect("spawn test thread")
    .join()
    .expect("test thread")
}

#[test]
fn left_nested_sync_flat_maps_are_stack_safe() {
  let n = depth();
  let out = on_small_stack(move || {
    let mut eff: Effect<u64> = succeed(0);
    for _ in 0..n {
      eff = eff.flat_map(|x| succeed(x + 1));
    }
    run(eff)
  });
  assert_eq!(out, Exit::succeed(n));
}

#[test]
fn left_nested_sync_maps_are_stack_safe() {
  let n = depth();
  let out = on_small_stack(move || {
    let mut eff: Effect<u64> = sync(|| 0);
    for _ in 0..n {
      eff = eff.map(|x| x + 1);
    }
    run(eff)
  });
  assert_eq!(out, Exit::succeed(n));
}

#[test]
fn left_nested_async_binds_are_stack_safe() {
  let n = depth();
  let out = on_small_stack(move || {
    let mut eff: Effect<u64> = from_async(|| async { Ok(0) });
    for _ in 0..n {
      eff = eff.flat_map(|x| from_async(move || async move { Ok(x + 1) }));
    }
    run(eff)
  });
  assert_eq!(out, Exit::succeed(n));
}

#[test]
fn right_nested_async_binds_are_stack_safe() {
  fn count(i: u64, n: u64) -> Effect<u64> {
    if i == n {
      succeed(i)
    } else {
      from_async(move || async move { Ok(i + 1) }).flat_map(move |j| count(j, n))
    }
  }
  let n = depth();
  let out = on_small_stack(move || run(count(0, n)));
  assert_eq!(out, Exit::succeed(n));
}

// ── AC5: interrupt checks ──────────────────────────────────────────────────────────────────

#[test]
fn an_interrupt_requested_before_driving_stops_every_step() {
  let steps = counter();
  let (s1, s2, s3) = (Arc::clone(&steps), Arc::clone(&steps), Arc::clone(&steps));
  let eff: Effect<u32> = from_async(move || async move {
    bump(&s1);
    Ok(1)
  })
  .flat_map(move |x| {
    bump(&s2);
    from_async(move || async move { Ok(x + 1) })
  })
  .map(move |x| {
    bump(&s3);
    x
  });
  let fiber = fiber();
  fiber.request_interrupt();
  let exit = pollster::block_on(eff.run_on(Arc::clone(&fiber)));
  assert_eq!(exit, Exit::interrupt(fiber.id()));
  assert_eq!(get(&steps), 0);
}

#[test]
fn no_step_runs_after_the_interrupt_is_observed() {
  let after = counter();
  let a2 = Arc::clone(&after);
  let eff: Effect<u32> = Effect::from_ctx(|ctx: Ctx<HNil>| async move {
    ctx.fiber().request_interrupt();
    Ok(1)
  })
  .flat_map(move |x| {
    bump(&a2);
    from_async(move || async move { Ok(x + 1) })
  });
  let fiber = fiber();
  let exit = pollster::block_on(eff.run_on(Arc::clone(&fiber)));
  assert_eq!(exit, Exit::interrupt(fiber.id()));
  assert_eq!(get(&after), 0);
}

/// Pending until the shared flag is set, waking through the stored waker.
struct Gate(Arc<Mutex<(bool, Option<Waker>)>>);

impl Future for Gate {
  type Output = ();
  fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
    let mut g = self.0.lock().unwrap();
    if g.0 {
      Poll::Ready(())
    } else {
      g.1 = Some(cx.waker().clone());
      Poll::Pending
    }
  }
}

#[test]
fn interrupting_a_suspended_fiber_wakes_it() {
  let gate = Arc::new(Mutex::new((false, None)));
  let g2 = Arc::clone(&gate);
  let eff: Effect<u32> = from_async(move || async move {
    Gate(g2).await;
    Ok(1)
  });
  let fiber = fiber();
  let f2 = Arc::clone(&fiber);
  let interrupter = std::thread::spawn(move || {
    // Wait (by polling the gate's waker slot) until the runloop has suspended, then interrupt.
    loop {
      if gate.lock().unwrap().1.is_some() {
        break;
      }
      std::thread::yield_now();
    }
    f2.request_interrupt();
  });
  let exit = pollster::block_on(eff.run_on(Arc::clone(&fiber)));
  interrupter.join().unwrap();
  assert_eq!(exit, Exit::interrupt(fiber.id()));
}

// ── AC6: panics are Die with the root Defect ───────────────────────────────────────────────

#[test]
fn a_panic_in_map_becomes_die_with_its_payload() {
  let eff: Effect<u32> = succeed(1).map(|_| -> u32 { panic!("boom") });
  match run(eff) {
    Exit::Failure(Cause::Die(d)) => {
      assert_eq!(d.message(), "boom");
      assert_eq!(d.with_payload(|s: &&str| *s), Some("boom"));
    }
    other => panic!("expected Die, got {other:?}"),
  }
}

#[test]
fn panics_in_async_steps_and_continuations_become_die() {
  let eff: Effect<u32> =
    from_async(|| async { Ok(1) }).flat_map(|_| -> Effect<u32> { panic!("in continuation") });
  assert!(matches!(run(eff), Exit::Failure(Cause::Die(d)) if d.message() == "in continuation"));
  let eff: Effect<u32> = from_async(|| async { panic!("in future") });
  assert!(matches!(run(eff), Exit::Failure(Cause::Die(d)) if d.message() == "in future"));
}

// ── core combinators ───────────────────────────────────────────────────────────────────────

#[test]
fn fail_die_and_short_circuit() {
  let c = counter();
  let c2 = Arc::clone(&c);
  let eff: Effect<u32, String> = fail::<u32, String, HNil>("no".into()).map(move |x| {
    bump(&c2);
    x
  });
  assert_eq!(run(eff), Exit::fail("no".to_owned()));
  assert_eq!(get(&c), 0);
  let eff: Effect<u32, String> = die("dead");
  assert_eq!(run(eff), Exit::die("dead"));
}

#[test]
fn map_err_catch_and_exit() {
  let eff: Effect<u32, u8> = from_async(|| async { Err(3_u8) });
  assert_eq!(run(eff.map_err(|e| u16::from(e) * 2)), Exit::fail(6_u16));

  let eff: Effect<u32, u8> = from_async(|| async { Err(3_u8) });
  let recovered: Effect<u32, String> = eff.catch(|e| succeed(u32::from(e) + 100));
  assert_eq!(run(recovered), Exit::succeed(103));

  let dead: Effect<u32, u8> = die("d");
  let not_caught: Effect<u32, String> = dead.catch(|_| succeed(0));
  assert_eq!(run(not_caught), Exit::die("d"));

  let pure_fail: Effect<u32, u8> = fail(1);
  assert_eq!(
    run(pure_fail.catch(|e| succeed::<u32, (), HNil>(u32::from(e)))),
    Exit::succeed(1)
  );

  let eff: Effect<u32, u8> = from_async(|| async { Err(9_u8) });
  let seen: Effect<Exit<u32, u8>> = eff.exit();
  assert_eq!(run(seen), Exit::succeed(Exit::fail(9_u8)));
  let seen: Effect<Exit<u32, u8>> = succeed::<u32, u8, HNil>(4).exit();
  assert_eq!(run(seen), Exit::succeed(Exit::succeed(4)));
}

#[test]
fn then_zip_and_widen() {
  let a: Effect<u32> = from_async(|| async { Ok(1) });
  let b: Effect<&'static str> = succeed("b");
  assert_eq!(run(a.zip(b)), Exit::succeed((1, "b")));
  let widened: Effect<u32, String> = succeed::<(), _, HNil>(()).then(succeed(5)).widen();
  assert_eq!(run(widened), Exit::succeed(5));
}

#[test]
fn sleep_uses_the_executor_and_yield_fiber_resumes() {
  let exec = Arc::new(StubExec::default());
  let fiber = FiberContext::new_root(Arc::clone(&exec) as Arc<dyn Executor>);
  let eff: Effect<u32> = sleep(Duration::from_secs(10))
    .then(yield_fiber())
    .then(succeed(1));
  assert_eq!(pollster::block_on(eff.run_on(fiber)), Exit::succeed(1));
  assert_eq!(*exec.slept.lock().unwrap(), vec![Duration::from_secs(10)]);
}

#[test]
fn from_ctx_sees_the_fiber() {
  let fiber = fiber();
  let id = fiber.id();
  let eff: Effect<bool> =
    Effect::from_ctx(move |ctx: Ctx<HNil>| async move { Ok(ctx.fiber().id() == id) });
  assert_eq!(pollster::block_on(eff.run_on(fiber)), Exit::succeed(true));
}

#[test]
fn many_steps_cross_the_yield_budget() {
  // > BUDGET (2048) instructions: the runloop yields to the executor and is re-polled.
  let mut eff: Effect<u64> = from_async(|| async { Ok(0) });
  for _ in 0..5_000 {
    eff = eff.flat_map(|x| from_async(move || async move { Ok(x + 1) }));
  }
  assert_eq!(run(eff), Exit::succeed(5_000));
}

#[test]
fn exit_kind_of_public_exits() {
  assert_eq!(
    ExitKind::of(&run(succeed::<u8, (), HNil>(1))),
    ExitKind::Success
  );
  assert_eq!(
    ExitKind::of(&run(fail::<u8, u8, HNil>(1))),
    ExitKind::Failure
  );
  assert_eq!(ExitKind::of(&run(die::<u8, u8, HNil>("x"))), ExitKind::Die);
}
