//! `k-executor`: `TokioExecutor` and `TestExecutor` behind the `next` kernel's `Executor` trait,
//! driven with plain futures (no `Effect` needed).

use std::cell::Cell;
use std::future::poll_fn;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Poll, Waker};
use std::time::{Duration, Instant};

use id_effect::next::{Executor, NotLocal, TestExecutor, TokioExecutor};

fn counter() -> Arc<AtomicUsize> {
  Arc::new(AtomicUsize::new(0))
}

// ── TokioExecutor ───────────────────────────────────────────────────────────────────────────

fn tokio_rt() -> tokio::runtime::Runtime {
  tokio::runtime::Builder::new_multi_thread()
    .worker_threads(2)
    .enable_time()
    .build()
    .expect("tokio runtime")
}

#[test]
fn tokio_current_is_none_outside_a_runtime() {
  assert!(TokioExecutor::current().is_none());
}

#[test]
fn tokio_current_is_some_inside_a_runtime() {
  let rt = tokio_rt();
  let _guard = rt.enter();
  assert!(TokioExecutor::current().is_some());
}

#[test]
fn tokio_spawn_runs_the_future() {
  let rt = tokio_rt();
  let exec = TokioExecutor::new(rt.handle().clone());
  let (tx, rx) = tokio::sync::oneshot::channel();
  exec.spawn(Box::pin(async move {
    let _ = tx.send(7_u8);
  }));
  assert_eq!(rt.block_on(rx).expect("sent"), 7);
}

#[test]
fn tokio_sleep_is_a_real_timer() {
  let rt = tokio_rt();
  let exec = TokioExecutor::new(rt.handle().clone());
  // Created outside the runtime context: `sleep` enters the handle itself.
  let sleep = exec.sleep(Duration::from_millis(30));
  let start = exec.now();
  rt.block_on(sleep);
  assert!(exec.now() - start >= Duration::from_millis(30));
}

#[test]
fn tokio_spawn_blocking_runs_on_another_thread() {
  let rt = tokio_rt();
  let exec = TokioExecutor::new(rt.handle().clone());
  let (tx, rx) = tokio::sync::oneshot::channel();
  exec.spawn_blocking(Box::new(move || {
    let _ = tx.send(std::thread::current().id());
  }));
  assert_ne!(rt.block_on(rx).expect("sent"), std::thread::current().id());
}

#[test]
fn tokio_spawn_local_is_not_local() {
  let rt = tokio_rt();
  let exec = TokioExecutor::new(rt.handle().clone());
  assert_eq!(exec.spawn_local(Box::pin(async {})), Err(NotLocal));
}

#[test]
fn tokio_executor_is_usable_as_a_trait_object() {
  let rt = tokio_rt();
  let exec: Arc<dyn Executor> = Arc::new(TokioExecutor::new(rt.handle().clone()));
  let (tx, rx) = tokio::sync::oneshot::channel();
  let e2 = Arc::clone(&exec);
  exec.spawn(Box::pin(async move {
    e2.sleep(Duration::from_millis(1)).await;
    let _ = tx.send(());
  }));
  rt.block_on(rx).expect("sent");
}

// ── TestExecutor ────────────────────────────────────────────────────────────────────────────

/// AC3: 1 000 tasks each sleeping 10 s complete after `advance(10s)`, in < 50 ms of wall time.
#[test]
fn thousand_ten_second_sleeps_finish_on_advance_quickly() {
  let wall = Instant::now();
  let exec = TestExecutor::new();
  let done = counter();
  for _ in 0..1_000 {
    let sleep = exec.sleep(Duration::from_secs(10));
    let done = Arc::clone(&done);
    exec.spawn(Box::pin(async move {
      sleep.await;
      done.fetch_add(1, Ordering::SeqCst);
    }));
  }
  exec.run_until_stalled();
  assert_eq!(done.load(Ordering::SeqCst), 0);
  exec.advance(Duration::from_secs(10) - Duration::from_nanos(1));
  assert_eq!(done.load(Ordering::SeqCst), 0);
  exec.advance(Duration::from_nanos(1));
  assert_eq!(done.load(Ordering::SeqCst), 1_000);
  assert_eq!(exec.pending_timers(), 0);
  let elapsed = wall.elapsed();
  assert!(elapsed < Duration::from_millis(50), "took {elapsed:?}");
}

fn mixed_sleep_order() -> Vec<usize> {
  let exec = TestExecutor::new();
  let order = Arc::new(Mutex::new(Vec::new()));
  for i in 0..100_usize {
    // Sleeps registered inside the task (on first poll), several tasks per deadline, and a
    // second sleep for some tasks.
    let exec2 = exec.clone();
    let order = Arc::clone(&order);
    exec.spawn(Box::pin(async move {
      exec2
        .sleep(Duration::from_millis(((i * 37) % 11) as u64 * 100))
        .await;
      if i % 3 == 0 {
        exec2
          .sleep(Duration::from_millis(((i * 13) % 7) as u64 * 50))
          .await;
      }
      order.lock().expect("order").push(i);
    }));
  }
  exec.advance(Duration::from_secs(2));
  assert_eq!(exec.pending_timers(), 0);
  order.lock().expect("order").clone()
}

/// AC4: the completion order of 100 tasks with mixed sleeps is identical across 50 runs.
#[test]
fn mixed_sleep_completion_order_is_deterministic() {
  let first = mixed_sleep_order();
  assert_eq!(first.len(), 100);
  for _ in 0..49 {
    assert_eq!(mixed_sleep_order(), first);
  }
}

/// AC4 (shape): sleeps fire in deadline order, ties in registration (FIFO) order.
#[test]
fn sleeps_fire_in_deadline_then_fifo_order() {
  let exec = TestExecutor::new();
  let order = Arc::new(Mutex::new(Vec::new()));
  for (i, ms) in [300_u64, 100, 200, 100, 300, 0].into_iter().enumerate() {
    let sleep = exec.sleep(Duration::from_millis(ms));
    let order = Arc::clone(&order);
    exec.spawn(Box::pin(async move {
      sleep.await;
      order.lock().expect("order").push(i);
    }));
  }
  exec.advance(Duration::from_millis(300));
  assert_eq!(*order.lock().expect("order"), vec![5, 1, 3, 2, 0, 4]);
}

/// AC5: a task that is never woken is polled exactly once by `run_until_stalled`.
#[test]
fn unwoken_task_is_polled_exactly_once() {
  let exec = TestExecutor::new();
  let polls = counter();
  let p = Arc::clone(&polls);
  exec.spawn(Box::pin(poll_fn(move |_| {
    p.fetch_add(1, Ordering::SeqCst);
    Poll::<()>::Pending
  })));
  assert_eq!(exec.run_until_stalled(), 1);
  assert_eq!(exec.run_until_stalled(), 0);
  exec.advance(Duration::from_secs(60));
  assert_eq!(polls.load(Ordering::SeqCst), 1);
}

#[test]
fn a_wake_from_another_thread_requeues_the_task() {
  let exec = TestExecutor::new();
  let slot: Arc<Mutex<Option<Waker>>> = Arc::new(Mutex::new(None));
  let polls = counter();
  let (s, p) = (Arc::clone(&slot), Arc::clone(&polls));
  exec.spawn(Box::pin(poll_fn(move |cx| {
    if p.fetch_add(1, Ordering::SeqCst) == 0 {
      *s.lock().expect("slot") = Some(cx.waker().clone());
      Poll::Pending
    } else {
      Poll::Ready(())
    }
  })));
  exec.run_until_stalled();
  assert_eq!(exec.queued_tasks(), 0);
  let waker = slot.lock().expect("slot").take().expect("waker stored");
  std::thread::spawn(move || {
    // Waking twice queues the task once.
    waker.wake_by_ref();
    waker.wake();
  })
  .join()
  .expect("waker thread");
  assert_eq!(exec.queued_tasks(), 1);
  assert_eq!(exec.run_until_stalled(), 1);
  assert_eq!(polls.load(Ordering::SeqCst), 2);
}

#[test]
fn now_is_virtual_and_moves_with_advance() {
  let exec = TestExecutor::new();
  let t0 = exec.now();
  let seen = Arc::new(Mutex::new(Vec::new()));
  let (e, s) = (exec.clone(), Arc::clone(&seen));
  exec.spawn(Box::pin(async move {
    e.sleep(Duration::from_secs(1)).await;
    s.lock().expect("seen").push(e.now());
    e.sleep(Duration::from_secs(2)).await;
    s.lock().expect("seen").push(e.now());
  }));
  exec.advance(Duration::from_secs(5));
  assert_eq!(
    *seen.lock().expect("seen"),
    vec![t0 + Duration::from_secs(1), t0 + Duration::from_secs(3)]
  );
  assert_eq!(exec.now(), t0 + Duration::from_secs(5));
  assert_eq!(exec.next_timer_deadline(), None);
}

#[test]
fn zero_sleep_completes_without_advance() {
  let exec = TestExecutor::new();
  let done = counter();
  let (e, d) = (exec.clone(), Arc::clone(&done));
  exec.spawn(Box::pin(async move {
    e.sleep(Duration::ZERO).await;
    d.fetch_add(1, Ordering::SeqCst);
  }));
  exec.run_until_stalled();
  assert_eq!(done.load(Ordering::SeqCst), 1);
  assert_eq!(exec.pending_timers(), 0);
}

#[test]
fn spawn_local_runs_non_send_futures_on_the_owner_thread() {
  let exec = TestExecutor::new();
  let hits = Rc::new(Cell::new(0_u32));
  let (e, h) = (exec.clone(), Rc::clone(&hits));
  exec
    .spawn_local(Box::pin(async move {
      e.sleep(Duration::from_secs(1)).await;
      h.set(h.get() + 1);
      // A local task may spawn another local task.
      let h2 = Rc::clone(&h);
      e.spawn_local(Box::pin(async move { h2.set(h2.get() + 10) }))
        .expect("owner thread");
    }))
    .expect("owner thread");
  exec.run_until_stalled();
  assert_eq!(hits.get(), 0);
  exec.advance(Duration::from_secs(1));
  assert_eq!(hits.get(), 11);
}

#[test]
fn spawn_local_off_the_owner_thread_is_not_local() {
  let exec = TestExecutor::new();
  let e = exec.clone();
  let r = std::thread::spawn(move || e.spawn_local(Box::pin(async {})))
    .join()
    .expect("thread");
  assert_eq!(r, Err(NotLocal));
}

#[test]
fn spawn_blocking_runs_when_driven() {
  let exec = TestExecutor::new();
  let done = counter();
  let d = Arc::clone(&done);
  exec.spawn_blocking(Box::new(move || {
    d.fetch_add(1, Ordering::SeqCst);
  }));
  assert_eq!(done.load(Ordering::SeqCst), 0);
  exec.run_until_stalled();
  assert_eq!(done.load(Ordering::SeqCst), 1);
}

#[test]
fn spawn_from_another_thread_is_queued() {
  let exec = TestExecutor::new();
  let done = counter();
  let (e, d) = (exec.clone(), Arc::clone(&done));
  std::thread::spawn(move || {
    e.spawn(Box::pin(async move {
      d.fetch_add(1, Ordering::SeqCst);
    }));
  })
  .join()
  .expect("thread");
  exec.run_until_stalled();
  assert_eq!(done.load(Ordering::SeqCst), 1);
}

#[test]
fn test_executor_is_usable_as_a_trait_object() {
  let exec = TestExecutor::new();
  let dyn_exec: Arc<dyn Executor> = Arc::new(exec.clone());
  let done = counter();
  let (e, d) = (Arc::clone(&dyn_exec), Arc::clone(&done));
  dyn_exec.spawn(Box::pin(async move {
    e.sleep(Duration::from_millis(5)).await;
    d.fetch_add(1, Ordering::SeqCst);
  }));
  exec.advance(Duration::from_millis(5));
  assert_eq!(done.load(Ordering::SeqCst), 1);
}

/// AC6: the test executor does not depend on the tokio crate.
#[test]
fn test_executor_source_does_not_mention_tokio() {
  for src in [
    include_str!("../src/testing/test_executor.rs"),
    include_str!("../src/testing/mod.rs"),
  ] {
    assert!(!src.contains("tokio"));
  }
}
