//! Kernel semantics the design promises (§1.3, §5 spike plan): Send-by-default, stack safety,
//! interrupt propagation, masks, finalizers on interrupt, panic → `Die` with payload, real wakers
//! for `block_on`, and `block_on` inside a task returning an error.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use kernel_a::{
  BlockOnInAsync, Cause, Concurrency, Effect, ExitKind, HNil, Never, Runtime, acquire_use_release,
  caps, effect, effect_local, fail, fork_all, from_async, sleep, succeed, sync,
  uninterruptible_mask,
};

fn ms(n: u64) -> Duration {
  Duration::from_millis(n)
}

fn assert_send<T: Send>() {}

#[test]
fn effect_is_send_by_default() {
  assert_send::<Effect<u8, (), caps!(u32, String)>>();
  assert_send::<Effect<u8, (), HNil>>();
}

#[test]
fn ready_values_need_no_runtime_work() {
  assert_eq!(succeed::<u32, Never, HNil>(7).block_on(), Ok(7));
  assert_eq!(
    fail::<u32, &str, HNil>("x").block_on(),
    Err(Cause::Fail("x"))
  );
}

#[test]
fn stack_safe_at_100k_left_nested_binds() {
  let mut e: Effect<u64, ()> = succeed(0);
  for _ in 0..100_000 {
    e = e.flat_map(|n| succeed(n + 1));
  }
  assert_eq!(e.block_on(), Ok(100_000));
  let mut m: Effect<u64, ()> = succeed(0);
  for _ in 0..100_000 {
    m = m.map(|n| n + 1);
  }
  assert_eq!(m.block_on(), Ok(100_000));
}

#[test]
fn stack_safe_with_async_steps() {
  let mut e: Effect<u64, ()> = succeed(0);
  for _ in 0..10_000 {
    e = e.flat_map(|n| from_async(move || async move { Ok(n + 1) }));
  }
  assert_eq!(e.block_on(), Ok(10_000));
}

#[test]
fn panic_becomes_die_with_payload() {
  let e: Effect<u32, ()> = sync(|| panic!("boom-payload"));
  match e.block_on() {
    Err(Cause::Die(d)) => {
      assert_eq!(d.message(), "boom-payload");
      assert!(d.payload().is_some());
    }
    other => panic!("expected Die, got {other:?}"),
  }
}

#[test]
fn panic_inside_effect_body_is_caught() {
  struct N(u32);
  fn h() -> Effect<u32, (), caps!(N)> {
    effect!({
      let n = ~N.0;
      if n == 3 {
        panic!("three");
      }
      n
    })
  }
  assert!(matches!(h().provide(N(3)).block_on(), Err(Cause::Die(_))));
  assert_eq!(h().provide(N(4)).block_on(), Ok(4));
}

#[test]
fn block_on_inside_a_task_is_an_error_not_a_deadlock() {
  let rt = Runtime::new().unwrap();
  let inner = rt.clone();
  let h = rt.fork(from_async::<_, (), _, _, _>(move || async move {
    Ok(inner.try_block_on(succeed::<u32, (), HNil>(1)).err())
  }));
  let out = rt.block_on_future(h.join_future()).unwrap();
  assert_eq!(out, Ok(Some(BlockOnInAsync)));
}

#[test]
fn run_is_the_async_edge() {
  let rt = Runtime::new().unwrap();
  let r2 = rt.clone();
  let h = rt.fork(from_async::<_, (), _, _, _>(move || async move {
    Ok(r2.run(sleep::<(), _>(ms(1)).map(|()| 5u32)).await)
  }));
  assert_eq!(rt.block_on_future(h.join_future()).unwrap(), Ok(Ok(5)));
}

#[test]
fn interrupt_stops_a_suspended_fiber_and_runs_its_finalizer() {
  let finalized = Arc::new(AtomicBool::new(false));
  let f2 = Arc::clone(&finalized);
  let started = Instant::now();
  let prog: Effect<ExitKind, ()> = sleep::<(), HNil>(Duration::from_secs(30))
    .ensuring(sync(move || f2.store(true, Ordering::SeqCst)))
    .fork()
    .widen()
    .flat_map(|h| sleep(ms(10)).flat_map(move |()| h.interrupt().map(|exit| ExitKind::of(&exit))));
  assert_eq!(prog.block_on(), Ok(ExitKind::Interrupt));
  assert!(finalized.load(Ordering::SeqCst));
  assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn parent_exit_interrupts_and_awaits_children() {
  let child_done = Arc::new(AtomicBool::new(false));
  let c2 = Arc::clone(&child_done);
  // The parent forks a long child and returns immediately; the child must be interrupted (its
  // finalizer runs) before the parent's exit is observed.
  let parent: Effect<(), ()> = sleep::<(), HNil>(Duration::from_secs(30))
    .ensuring(sync(move || c2.store(true, Ordering::SeqCst)))
    .fork()
    .widen()
    // Give the child time to enter its `ensuring` region: a child interrupted before its first
    // instruction never runs its finalizer (it never acquired anything), as in Effect.ts.
    .flat_map(|_handle| sleep(ms(10)));
  let rt = Runtime::new().unwrap();
  let h = rt.fork(parent);
  assert_eq!(rt.block_on_future(h.join_future()).unwrap(), Ok(()));
  assert!(child_done.load(Ordering::SeqCst));
}

#[test]
fn fork_daemon_survives_its_parent() {
  let flag = Arc::new(AtomicBool::new(false));
  let f2 = Arc::clone(&flag);
  let parent: Effect<kernel_a::FiberHandle<(), ()>, ()> = sleep::<(), HNil>(ms(20))
    .map(move |()| f2.store(true, Ordering::SeqCst))
    .fork_daemon()
    .widen();
  let h = parent.block_on().unwrap();
  let rt = Runtime::global();
  assert_eq!(rt.block_on_future(h.join_future()).unwrap(), Ok(()));
  assert!(flag.load(Ordering::SeqCst));
}

#[test]
fn uninterruptible_region_defers_the_interrupt() {
  let reached = Arc::new(AtomicU32::new(0));
  let r2 = Arc::clone(&reached);
  let r3 = Arc::clone(&reached);
  let body: Effect<(), ()> = uninterruptible_mask(move |restore| {
    sleep::<(), HNil>(ms(30))
      .map(move |()| {
        r2.fetch_add(1, Ordering::SeqCst);
      })
      .then(restore.apply(sleep(Duration::from_secs(30))))
      .map(move |()| {
        r3.fetch_add(100, Ordering::SeqCst);
      })
  });
  let prog: Effect<ExitKind, ()> = body
    .fork()
    .widen()
    .flat_map(|h| sleep(ms(5)).flat_map(move |()| h.interrupt().map(|e| ExitKind::of(&e))));
  assert_eq!(prog.block_on(), Ok(ExitKind::Interrupt));
  // The masked 30 ms sleep finished despite the early interrupt; the restored region did not.
  assert_eq!(reached.load(Ordering::SeqCst), 1);
}

#[test]
fn release_runs_on_failure_and_on_interrupt() {
  let kinds = Arc::new(std::sync::Mutex::new(Vec::new()));
  let k1 = Arc::clone(&kinds);
  let failing: Effect<u32, &str> = acquire_use_release(
    succeed(1u32),
    |_| fail("use failed"),
    move |_, kind| sync(move || k1.lock().unwrap().push(kind)),
  );
  assert_eq!(failing.block_on(), Err(Cause::Fail("use failed")));
  let k2 = Arc::clone(&kinds);
  let hanging: Effect<(), ()> = acquire_use_release(
    succeed(1u32),
    |_| sleep(Duration::from_secs(30)),
    move |_, kind| sync(move || k2.lock().unwrap().push(kind)),
  );
  assert_eq!(hanging.timeout(ms(10)).block_on(), Ok(None));
  assert_eq!(
    *kinds.lock().unwrap(),
    vec![ExitKind::Failure, ExitKind::Interrupt]
  );
}

#[test]
fn fork_all_first_failure_interrupts_siblings() {
  let started = Instant::now();
  let effs: Vec<Effect<u32, &str>> = vec![
    sleep(Duration::from_secs(30)).map(|()| 1),
    sleep(ms(5)).then(fail("second failed")),
    sleep(Duration::from_secs(30)).map(|()| 3),
  ];
  assert_eq!(
    fork_all(effs, Concurrency::Unbounded).block_on(),
    Err(Cause::Fail("second failed"))
  );
  assert!(started.elapsed() < Duration::from_secs(5));
  let ok: Vec<Effect<u32, ()>> = (0..10).map(|i| sleep(ms(1)).map(move |()| i)).collect();
  assert_eq!(
    fork_all(ok, Concurrency::Bounded(3)).block_on(),
    Ok((0..10).collect())
  );
}

#[test]
fn sleeping_fibers_are_parked_not_spinning() {
  // 200 fibers sleeping 50 ms: CPU time must stay far below 200 x 50 ms of spinning.
  let rt = Runtime::new().unwrap();
  let cpu = cpu_time::ProcessTime::now();
  rt.block_on_future(async {
    let hs: Vec<_> = (0..200).map(|_| rt.fork(sleep::<(), _>(ms(50)))).collect();
    for h in hs {
      h.join_future().await.unwrap();
    }
  })
  .unwrap();
  assert!(
    cpu.elapsed() < Duration::from_secs(1),
    "cpu {:?}",
    cpu.elapsed()
  );
}

#[test]
fn local_effect_holds_non_send_state_across_awaits() {
  use std::rc::Rc;
  fn local() -> kernel_a::LocalEffect<usize> {
    effect_local!({
      let shared = Rc::new(String::from("ab"));
      ~kernel_a::yield_now::<Never, HNil>();
      let n = ~succeed::<u32, Never, HNil>(1);
      shared.len() + n as usize
    })
  }
  assert_eq!(local().block_on(), Ok(3));
}
