//! `k-edge`: the run edges of `id_effect` (`Runtime::run`, `try_block_on`/`block_on` on a
//! parking waker, `BlockOnInAsync`) and `LocalEffect`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use id_effect::{
  BlockOnInAsync, BoxFuture, Effect, Executor, FiberContext, HCons, HNil, Here, LocalBoxFuture,
  LocalEffect, NotLocal, Runtime, Selector, TestExecutor, TokioExecutor, fail, sleep, succeed,
  sync,
};
use id_effect::{Cause, Exit};

type Never = std::convert::Infallible;

fn ms(n: u64) -> Duration {
  Duration::from_millis(n)
}

/// Counts every executor call, delegating to an inner executor.
struct Counting {
  inner: Arc<dyn Executor>,
  calls: AtomicUsize,
}

impl Counting {
  fn new(inner: Arc<dyn Executor>) -> Arc<Self> {
    Arc::new(Self {
      inner,
      calls: AtomicUsize::new(0),
    })
  }

  fn calls(&self) -> usize {
    self.calls.load(Ordering::SeqCst)
  }

  fn bump(&self) {
    self.calls.fetch_add(1, Ordering::SeqCst);
  }
}

impl Executor for Counting {
  fn spawn(&self, fut: BoxFuture<'static, ()>) {
    self.bump();
    self.inner.spawn(fut);
  }
  fn spawn_local(&self, fut: LocalBoxFuture<'static, ()>) -> Result<(), NotLocal> {
    self.bump();
    self.inner.spawn_local(fut)
  }
  fn spawn_blocking(&self, f: Box<dyn FnOnce() + Send>) {
    self.bump();
    self.inner.spawn_blocking(f);
  }
  fn sleep(&self, d: Duration) -> BoxFuture<'static, ()> {
    self.bump();
    self.inner.sleep(d)
  }
  fn now(&self) -> Instant {
    self.bump();
    self.inner.now()
  }
}

fn counting_runtime() -> (Runtime, Arc<Counting>) {
  let exec = Counting::new(Arc::clone(Runtime::global().executor()));
  (
    Runtime::from_executor(Arc::clone(&exec) as Arc<dyn Executor>),
    exec,
  )
}

/// Process CPU time (user + system) from `/proc/self/stat`, in clock ticks of 10 ms.
fn process_cpu() -> Duration {
  let stat = std::fs::read_to_string("/proc/self/stat").expect("procfs");
  // Fields after the `(comm)` field: state is field 3; utime/stime are fields 14/15.
  let rest = &stat[stat.rfind(')').expect("comm") + 2..];
  let f: Vec<&str> = rest.split_whitespace().collect();
  let ticks: u64 = f[11].parse::<u64>().unwrap() + f[12].parse::<u64>().unwrap();
  Duration::from_millis(ticks * 10)
}

/// CPU time of the calling thread (`/proc/thread-self/schedstat`, nanoseconds).
fn thread_cpu() -> Duration {
  let s = std::fs::read_to_string("/proc/thread-self/schedstat").expect("procfs");
  Duration::from_nanos(s.split_whitespace().next().unwrap().parse().unwrap())
}

// ---------------------------------------------------------------- AC1: the edges

#[test]
fn block_on_runs_async_effects_to_completion() {
  let eff = sleep::<Never, HNil>(ms(5))
    .map(|()| 20_u32)
    .flat_map(|n| succeed(n + 1))
    .flat_map(|n| sync(move || n * 2));
  assert_eq!(Runtime::global().block_on(eff), Exit::Success(42));
  assert_eq!(
    sleep::<&str, HNil>(ms(1))
      .then(fail::<u8, _, _>("no"))
      .block_on(),
    Exit::Failure(Cause::Fail("no"))
  );
}

#[test]
fn effect_block_on_is_a_convenience_over_global() {
  let eff: Effect<u32> = sleep(ms(1)).map(|()| 7);
  assert_eq!(eff.block_on(), Exit::Success(7));
  let eff: Effect<u32> = succeed(8);
  assert_eq!(eff.try_block_on(), Ok(Exit::Success(8)));
}

#[test]
fn runtime_new_owns_its_tokio_runtime() {
  let rt = Runtime::new().expect("runtime");
  let eff: Effect<u32> = sleep(ms(2)).map(|()| 3);
  assert_eq!(rt.block_on(eff), Exit::Success(3));
  let clone = rt.clone();
  drop(rt);
  let eff: Effect<u32> = sleep(ms(1)).map(|()| 4);
  assert_eq!(clone.block_on(eff), Exit::Success(4));
}

#[test]
fn block_on_future_parks_on_any_future() {
  let rt = Runtime::new().expect("runtime");
  assert!(TokioExecutor::current().is_none(), "not inside tokio here");
  let out = rt.block_on_future(rt.run(sleep::<Never, HNil>(ms(2)).map(|()| 9_u8)));
  assert_eq!(out, Ok(Exit::Success(9)));
}

#[test]
fn block_on_interrupts_and_awaits_the_roots_children() {
  let finalized = Arc::new(AtomicUsize::new(0));
  let f2 = Arc::clone(&finalized);
  let child: Effect<(), Never> =
    sleep::<Never, HNil>(Duration::from_secs(30)).ensuring(sync(move || {
      f2.fetch_add(1, Ordering::SeqCst);
    }));
  let started = Instant::now();
  // The root outlives its own program only by its child: it returns after 50 ms, while the
  // child (started by then) still sleeps.
  let root: Effect<u8> = child.fork().flat_map(|_handle| sleep(ms(50))).map(|()| 1);
  assert_eq!(root.block_on(), Exit::Success(1));
  assert_eq!(
    finalized.load(Ordering::SeqCst),
    1,
    "child finalized before the edge returned"
  );
  assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn runtime_fork_starts_a_daemon_root_fiber() {
  let rt = Runtime::new().expect("runtime");
  let h = rt.fork(sleep::<Never, HNil>(ms(2)).map(|()| 11_u8));
  assert_eq!(rt.block_on_future(h.join_future()), Ok(Exit::Success(11)));
}

#[test]
fn panics_in_fused_steps_become_die() {
  let eff: Effect<u32> = sync(|| panic!("boom"));
  match eff.block_on() {
    Exit::Failure(Cause::Die(d)) => assert_eq!(d.message(), "boom"),
    other => panic!("expected Die, got {other:?}"),
  }
}

// ---------------------------------------------------------------- AC2: BlockOnInAsync

fn assert_refused_fast() {
  let started = Instant::now();
  let rt = Runtime::global();
  let eff: Effect<(), Never> = sleep(Duration::from_secs(30));
  assert_eq!(rt.try_block_on(eff), Err(BlockOnInAsync));
  assert_eq!(
    rt.block_on_future(std::future::pending::<()>()),
    Err(BlockOnInAsync)
  );
  let eff: Effect<u32> = succeed(1);
  assert_eq!(eff.try_block_on(), Err(BlockOnInAsync));
  match sleep::<Never, HNil>(Duration::from_secs(30)).block_on() {
    Exit::Failure(Cause::Die(d)) => assert!(d.message().contains("async context"), "{d:?}"),
    other => panic!("expected Die, got {other:?}"),
  }
  let local: LocalEffect<u32> = LocalEffect::succeed(1);
  assert_eq!(local.try_block_on(), Err(BlockOnInAsync));
  assert!(started.elapsed() < ms(100), "{:?}", started.elapsed());
}

#[tokio::test(flavor = "current_thread")]
async fn try_block_on_in_current_thread_runtime_is_refused() {
  assert_refused_fast();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn try_block_on_in_multi_thread_runtime_is_refused() {
  assert_refused_fast();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn try_block_on_inside_a_spawned_task_is_refused() {
  tokio::spawn(async { assert_refused_fast() })
    .await
    .expect("task");
}

#[tokio::test(flavor = "current_thread")]
async fn run_is_the_async_edge() {
  // On the ambient (current-thread) runtime: its timer drives the sleep.
  let rt = Runtime::from_executor(Arc::new(TokioExecutor::current().expect("in tokio")));
  let eff: Effect<u32> = sleep(ms(2)).map(|()| 5);
  assert_eq!(rt.run(eff).await, Exit::Success(5));
  // And on the global runtime from inside another runtime.
  let eff: Effect<u32> = sleep(ms(1)).map(|()| 6);
  assert_eq!(Runtime::global().run(eff).await, Exit::Success(6));
}

#[test]
fn run_future_is_send_and_static() {
  fn assert_send_static<T: Send + 'static>(_: &T) {}
  let fut = Runtime::global().run(sleep::<Never, HNil>(ms(1)));
  assert_send_static(&fut);
  assert_eq!(
    Runtime::global().block_on_future(fut),
    Ok(Exit::Success(()))
  );
}

// ---------------------------------------------------------------- AC3: Pure touches nothing

#[test]
fn pure_and_fused_effects_never_touch_the_executor() {
  let (rt, exec) = counting_runtime();
  assert_eq!(
    rt.block_on(succeed::<u32, Never, HNil>(7)),
    Exit::Success(7)
  );
  assert_eq!(
    rt.block_on(fail::<u32, &str, HNil>("x")),
    Exit::Failure(Cause::Fail("x"))
  );
  let fused: Effect<u32> = succeed(1)
    .map(|n: u32| n + 1)
    .flat_map(|n| sync(move || n * 10));
  assert_eq!(rt.block_on(fused), Exit::Success(20));
  assert_eq!(
    pollster::block_on(rt.run(succeed::<u8, Never, HNil>(3))),
    Exit::Success(3)
  );
  assert_eq!(exec.calls(), 0, "spawn/sleep/now counter");
  // Sanity: a real async step does reach the executor.
  assert_eq!(rt.block_on(sleep::<Never, HNil>(ms(1))), Exit::Success(()));
  assert_eq!(exec.calls(), 1);
}

// ---------------------------------------------------------------- AC4: parked, not spinning

#[test]
fn block_on_parks_the_calling_thread_while_sleeping() {
  let eff: Effect<()> = sleep(ms(300));
  let (cpu, wall) = (thread_cpu(), Instant::now());
  assert_eq!(eff.block_on(), Exit::Success(()));
  let (cpu, wall) = (thread_cpu() - cpu, wall.elapsed());
  eprintln!("block_on(sleep 300ms): wall {wall:?}, calling-thread cpu {cpu:?}");
  assert!(wall >= ms(300), "{wall:?}");
  assert!(cpu < ms(20), "calling thread spun: {cpu:?}");
}

#[test]
fn sleeping_fibers_are_parked_not_spinning() {
  let tokio_rt = tokio::runtime::Builder::new_multi_thread()
    .worker_threads(4)
    .enable_time()
    .build()
    .expect("tokio");
  let rt = Runtime::from_executor(Arc::new(TokioExecutor::new(tokio_rt.handle().clone())));
  let (cpu, wall) = (process_cpu(), Instant::now());
  let handles: Vec<_> = (0..1_000)
    .map(|_| tokio_rt.spawn(rt.run(sleep::<Never, HNil>(ms(1)))))
    .collect();
  let done = rt
    .block_on_future(async move {
      let mut n = 0;
      for h in handles {
        assert_eq!(h.await.expect("join"), Exit::Success(()));
        n += 1;
      }
      n
    })
    .expect("not in async");
  let (cpu, wall) = (process_cpu() - cpu, wall.elapsed());
  eprintln!("1000 fibers x sleep(1ms): wall {wall:?}, process cpu {cpu:?}");
  assert_eq!(done, 1_000);
  assert!(cpu < ms(50), "process cpu {cpu:?}");
}

// ---------------------------------------------------------------- AC5: LocalEffect

#[test]
fn local_effect_holds_rc_refcell_across_awaits() {
  let log = Rc::new(RefCell::new(Vec::new()));
  let l2 = Rc::clone(&log);
  let eff = LocalEffect::<usize>::from_future(move || async move {
    l2.borrow_mut().push("start");
    Runtime::global().run(sleep::<Never, HNil>(ms(1))).await;
    l2.borrow_mut().push("after first await");
    Runtime::global().run(sleep::<Never, HNil>(ms(1))).await;
    l2.borrow_mut().push("after second await");
    Ok(l2.borrow().len())
  });
  assert_eq!(eff.block_on(), Exit::Success(3));
  assert_eq!(
    *log.borrow(),
    ["start", "after first await", "after second await"]
  );
}

#[test]
fn local_effect_has_the_full_combinator_set() {
  let rc = Rc::new(Cell::new(0_u32));
  let r2 = Rc::clone(&rc);
  let eff: LocalEffect<u32, String> = LocalEffect::sync(move || {
    r2.set(r2.get() + 1);
    Rc::new(10_u32)
  })
  .flat_map(|n| LocalEffect::sleep(ms(1)).map(move |()| *n))
  .zip(LocalEffect::succeed(5))
  .map(|(a, b)| a + b)
  .then(LocalEffect::<u32, String>::fail("bad".into()))
  .map_err(|e| format!("{e}!"))
  .catch(|e| LocalEffect::succeed(u32::try_from(e.len()).unwrap()));
  assert_eq!(eff.block_on(), Exit::Success(4));
  assert_eq!(rc.get(), 1);

  let exit = LocalEffect::<u8, &str>::fail("x")
    .exit::<Never>()
    .block_on();
  assert_eq!(exit, Exit::Success(Exit::Failure(Cause::Fail("x"))));
  let widened: LocalEffect<u8, &str> = LocalEffect::<u8>::succeed(1).widen();
  assert_eq!(widened.block_on(), Exit::Success(1));
  assert_eq!(
    LocalEffect::<u8>::suspend(|| LocalEffect::succeed(2)).block_on(),
    Exit::Success(2)
  );
  assert_eq!(
    LocalEffect::<(), Never>::yield_fiber()
      .map(|()| 3_u8)
      .block_on(),
    Exit::Success(3)
  );
}

#[test]
fn local_effect_deep_chains_use_frames_not_the_native_stack() {
  let mut eff: LocalEffect<u64> = LocalEffect::sleep(ms(1)).map(|()| 0);
  for _ in 0..100_000 {
    eff = eff.flat_map(|n| LocalEffect::succeed(n + 1));
  }
  assert_eq!(eff.block_on(), Exit::Success(100_000));
}

#[test]
fn local_effect_captures_panics_as_die() {
  let eff: LocalEffect<u8> = LocalEffect::sleep(ms(1)).map(|()| panic!("local boom"));
  match eff.block_on() {
    Exit::Failure(Cause::Die(d)) => assert_eq!(d.message(), "local boom"),
    other => panic!("expected Die, got {other:?}"),
  }
}

fn interrupt_after(fiber: &Arc<FiberContext>, d: Duration) {
  let f = Arc::clone(fiber);
  std::thread::spawn(move || {
    std::thread::sleep(d);
    f.request_interrupt();
  });
}

#[test]
fn local_effect_is_interruptible_and_runs_finalizers() {
  let finalized = Rc::new(Cell::new(false));
  let f2 = Rc::clone(&finalized);
  let fiber = FiberContext::new_root(Arc::clone(Runtime::global().executor()));
  let eff = LocalEffect::<(), Never>::sleep(Duration::from_secs(30))
    .ensuring(LocalEffect::sync(move || f2.set(true)));
  let started = Instant::now();
  interrupt_after(&fiber, ms(20));
  let exit = Runtime::global()
    .block_on_future(eff.run_on(Arc::clone(&fiber)))
    .expect("not in async");
  assert_eq!(exit, Exit::Failure(Cause::Interrupt(fiber.id())));
  assert!(finalized.get(), "ensuring ran");
  assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn local_uninterruptible_defers_the_interrupt() {
  let after = Rc::new(Cell::new(0_u8));
  let a2 = Rc::clone(&after);
  let fiber = FiberContext::new_root(Arc::clone(Runtime::global().executor()));
  let eff = LocalEffect::<(), Never>::sleep(ms(100))
    .map(|()| 1_u8)
    .uninterruptible()
    .map(move |n| a2.set(n));
  interrupt_after(&fiber, ms(10));
  let exit = Runtime::global()
    .block_on_future(eff.run_on(Arc::clone(&fiber)))
    .expect("not in async");
  assert_eq!(exit, Exit::Failure(Cause::Interrupt(fiber.id())));
  assert_eq!(after.get(), 0, "no step runs after the region exits");
}

#[test]
fn local_effect_provide_and_send_effects_inside() {
  type Env = HCons<u32, HNil>;
  let eff: LocalEffect<u32, Never, Env> = LocalEffect::from_ctx(|ctx| async move {
    let n: u32 = **<Env as Selector<u32, Here>>::get(ctx.env());
    Ok(n + 1)
  });
  let provided: LocalEffect<u32> = eff.provide(41_u32);
  assert_eq!(provided.block_on(), Exit::Success(42));

  let env_eff: LocalEffect<u32, Never, Env> =
    LocalEffect::from_ctx(|ctx| async move { Ok(**<Env as Selector<u32, Here>>::get(ctx.env())) });
  assert_eq!(
    env_eff.provide_env(HCons::new(7, HNil)).block_on(),
    Exit::Success(7)
  );

  // `Effect -> LocalEffect` is free and keeps async `Send` steps working.
  let send: Effect<u32> = sleep(ms(1)).map(|()| 3);
  let mixed = send
    .local()
    .flat_map(|n| LocalEffect::succeed(Rc::new(n)))
    .map(|rc| *rc * 2);
  assert_eq!(mixed.block_on(), Exit::Success(6));
}

#[test]
fn local_effect_interrupt_reaches_a_nested_send_effect() {
  let fiber = FiberContext::new_root(Arc::clone(Runtime::global().executor()));
  let send: Effect<(), Never> = sleep(Duration::from_secs(30));
  interrupt_after(&fiber, ms(20));
  let exit = Runtime::global()
    .block_on_future(send.local().run_on(Arc::clone(&fiber)))
    .expect("not in async");
  assert_eq!(exit, Exit::Failure(Cause::Interrupt(fiber.id())));
}

#[test]
fn run_on_local_needs_a_local_worker() {
  // Tokio: no `LocalSet` integration yet.
  let tokio_exec = Arc::clone(Runtime::global().executor());
  let eff: LocalEffect<u8> = LocalEffect::succeed(1);
  assert!(matches!(eff.run_on_local(&tokio_exec), Err(NotLocal)));

  // TestExecutor: `spawn_local` works on its owner thread.
  let test = TestExecutor::new();
  let exec: Arc<dyn Executor> = Arc::new(test.clone());
  let rc = Rc::new(Cell::new(0_u8));
  let r2 = Rc::clone(&rc);
  let local = LocalEffect::<(), Never>::sleep(Duration::from_secs(10)).map(move |()| {
    r2.set(r2.get() + 1);
    40
  });
  let send: Effect<u8> = local.run_on_local(&exec).expect("local worker");
  let out = Arc::new(Mutex::new(None));
  let o2 = Arc::clone(&out);
  let rt = Runtime::from_executor(Arc::clone(&exec));
  exec.spawn(Box::pin(async move {
    *o2.lock().unwrap() = Some(rt.run(send.map(|n| n + 2)).await);
  }));
  test.run_until_stalled();
  assert!(out.lock().unwrap().is_none(), "still sleeping (virtual)");
  test.advance(Duration::from_secs(10));
  test.run_until_stalled();
  assert_eq!(*out.lock().unwrap(), Some(Exit::Success(42)));
  assert_eq!(rc.get(), 1);
}

#[test]
fn dropping_a_run_on_local_effect_interrupts_the_worker() {
  let test = TestExecutor::new();
  let exec: Arc<dyn Executor> = Arc::new(test.clone());
  let finalized = Rc::new(Cell::new(false));
  let f2 = Rc::clone(&finalized);
  let local = LocalEffect::<(), Never>::sleep(Duration::from_secs(3600))
    .ensuring(LocalEffect::sync(move || f2.set(true)));
  let send = local.run_on_local(&exec).expect("local worker");
  test.run_until_stalled();
  drop(send);
  test.run_until_stalled();
  assert!(
    finalized.get(),
    "worker interrupted, finalizer ran without advancing time"
  );
}
