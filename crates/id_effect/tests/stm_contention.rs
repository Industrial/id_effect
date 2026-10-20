//! `m-fp` acceptance: STM on the 0.7 kernel under contention.
//!
//! Two fibers run 10 000 transactions each against one `TRef`:
//! - **increments** (`update_stm`) collide at commit validation and restart;
//! - **ping-pong** turns (`retry` until it is this fiber's turn) park the fiber on the cell's
//!   wait list until the other fiber commits; a lost wake-up would hang the test.
//!
//! Both run on a multi-thread tokio runtime (real parallelism, guarded by a deadline) and on the
//! deterministic single-threaded `TestExecutor`, where a spinning retry would never stall.

use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::sync::mpsc;
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use id_effect::Exit;
use id_effect::stm::{Stm, TRef, commit};
use id_effect::{Effect, FiberHandle, HNil, Runtime, TestExecutor, succeed, suspend};

const N: u32 = 10_000;

type Step = Arc<dyn Fn() -> Effect<(), ()> + Send + Sync>;

/// Runs `step` `n` times in sequence.
fn times(n: u32, step: Step) -> Effect<(), ()> {
  suspend(move || {
    if n == 0 {
      succeed(())
    } else {
      let next = Arc::clone(&step);
      step().flat_map(move |()| times(n - 1, next))
    }
  })
}

fn cell(v: u64) -> TRef<u64> {
  commit(TRef::make(v))
    .block_on()
    .into_result()
    .expect("cell")
}

fn read(c: &TRef<u64>) -> u64 {
  commit(c.read_stm::<()>())
    .block_on()
    .into_result()
    .expect("read")
}

/// `n` increments of `c`, one transaction each.
fn increments(c: &TRef<u64>, n: u32) -> Effect<(), ()> {
  let c = c.clone();
  times(
    n,
    Arc::new(move || commit(c.update_stm::<(), _>(|x| x + 1))),
  )
}

/// One turn: waits (STM retry) until `c` has parity `parity`, then increments it.
fn take_turn(c: &TRef<u64>, parity: u64) -> Stm<(), ()> {
  let c2 = c.clone();
  c.read_stm::<()>().flat_map(move |v| {
    if v % 2 == parity {
      c2.write_stm(v + 1)
    } else {
      Stm::<(), ()>::retry()
    }
  })
}

/// `n` strictly alternating turns for the fiber of `parity`.
fn turns(c: &TRef<u64>, parity: u64, n: u32) -> Effect<(), ()> {
  let c = c.clone();
  times(n, Arc::new(move || commit(take_turn(&c, parity))))
}

/// Runs `f` on a helper thread and fails the test if it does not finish in time (a lost
/// wake-up parks a fiber forever).
fn with_deadline<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
  let (tx, rx) = mpsc::channel();
  std::thread::spawn(move || {
    let _ = tx.send(f());
  });
  rx.recv_timeout(Duration::from_secs(120))
    .expect("contention run did not finish: a parked transaction was never woken")
}

/// Polls `fut` once; it must be ready.
fn take_ready<F: Future>(fut: F) -> F::Output {
  let mut fut = pin!(fut);
  match fut.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
    Poll::Ready(v) => v,
    Poll::Pending => panic!("future is not ready"),
  }
}

/// Forks both effects as root fibers on a fresh `TestExecutor` and runs it until it stalls.
fn on_test_executor(a: Effect<(), ()>, b: Effect<(), ()>) -> [FiberHandle<(), ()>; 2] {
  let exec = TestExecutor::new();
  let ha = a.fork_root(Arc::new(exec.clone()));
  let hb = b.fork_root(Arc::new(exec.clone()));
  exec.run_until_stalled();
  [ha, hb]
}

/// Forks both effects on an owned multi-thread tokio runtime and joins them.
fn on_tokio(a: Effect<(), ()>, b: Effect<(), ()>) -> [Exit<(), ()>; 2] {
  with_deadline(move || {
    let rt = Runtime::new().expect("tokio runtime");
    let ha = rt.fork(a);
    let hb = rt.fork(b);
    [
      rt.block_on(ha.join::<HNil>()),
      rt.block_on(hb.join::<HNil>()),
    ]
  })
}

#[test]
fn two_fibers_ten_thousand_increments_each_on_tokio_multi_thread() {
  let c = cell(0);
  let exits = on_tokio(increments(&c, N), increments(&c, N));
  assert_eq!(exits, [Exit::succeed(()), Exit::succeed(())]);
  assert_eq!(read(&c), 2 * u64::from(N), "no lost update");
}

#[test]
fn two_fibers_ten_thousand_retrying_turns_each_on_tokio_multi_thread() {
  let c = cell(0);
  let exits = on_tokio(turns(&c, 0, N), turns(&c, 1, N));
  assert_eq!(exits, [Exit::succeed(()), Exit::succeed(())]);
  assert_eq!(read(&c), 2 * u64::from(N));
}

#[test]
fn two_fibers_ten_thousand_increments_each_on_test_executor() {
  let c = cell(0);
  let [a, b] = on_test_executor(increments(&c, N), increments(&c, N));
  assert!(a.is_done() && b.is_done());
  assert_eq!(take_ready(a.join_future()), Exit::succeed(()));
  assert_eq!(take_ready(b.join_future()), Exit::succeed(()));
  assert_eq!(read(&c), 2 * u64::from(N));
}

#[test]
fn two_fibers_ten_thousand_retrying_turns_each_on_test_executor() {
  // Single-threaded and deterministic: every turn but the first parks its fiber until the
  // other one commits. With a spinning retry the executor would never stall.
  let c = cell(0);
  let [a, b] = on_test_executor(turns(&c, 0, N), turns(&c, 1, N));
  assert!(a.is_done() && b.is_done(), "both fibers finished all turns");
  assert_eq!(take_ready(a.join_future()), Exit::succeed(()));
  assert_eq!(take_ready(b.join_future()), Exit::succeed(()));
  assert_eq!(read(&c), 2 * u64::from(N));
}
