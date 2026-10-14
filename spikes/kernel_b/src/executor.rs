//! `Executor` (design §1.3), the built-in `ParkExecutor` behind `block_on` (thread-park waker +
//! timer thread, builds without tokio), the tokio backend, and `Runtime`.
//!
//! No `Waker::noop`, no `yield_now` spin: an idle `block_on` thread parks, a sleeping fiber is
//! a timer entry.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, Thread};
use std::time::{Duration, Instant};

use crate::cause::Exit;
use crate::fiber::{self, FiberHandle};
use crate::kernel::{BoxFuture, Effect, LocalEffect};

/// Where fibers run. Tokio is the default backend; `ParkExecutor` serves `block_on`.
pub trait Executor: Send + Sync + 'static {
  fn spawn(&self, fiber: BoxFuture<'static, ()>);
  fn sleep(&self, d: Duration) -> BoxFuture<'static, ()>;
  fn now(&self) -> Instant {
    Instant::now()
  }
}

// ── timer thread ────────────────────────────────────────────────────────────────────────────

struct SleepShared {
  done: AtomicBool,
  waker: Mutex<Option<Waker>>,
}

struct TimerEntry {
  at: Instant,
  seq: u64,
  shared: Arc<SleepShared>,
}

impl PartialEq for TimerEntry {
  fn eq(&self, o: &Self) -> bool {
    (self.at, self.seq) == (o.at, o.seq)
  }
}
impl Eq for TimerEntry {}
impl PartialOrd for TimerEntry {
  fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
    Some(self.cmp(o))
  }
}
impl Ord for TimerEntry {
  fn cmp(&self, o: &Self) -> std::cmp::Ordering {
    (self.at, self.seq).cmp(&(o.at, o.seq))
  }
}

struct Timer {
  heap: Mutex<(BinaryHeap<Reverse<TimerEntry>>, u64)>,
  cv: Condvar,
}

fn timer() -> &'static Timer {
  static TIMER: OnceLock<&'static Timer> = OnceLock::new();
  TIMER.get_or_init(|| {
    let t: &'static Timer = Box::leak(Box::new(Timer {
      heap: Mutex::new((BinaryHeap::new(), 0)),
      cv: Condvar::new(),
    }));
    thread::Builder::new()
      .name("kernel_b-timer".into())
      .spawn(move || timer_loop(t))
      .expect("spawn timer thread");
    t
  })
}

fn timer_loop(t: &'static Timer) {
  let mut guard = t.heap.lock().expect("timer lock");
  loop {
    let now = Instant::now();
    while guard.0.peek().is_some_and(|e| e.0.at <= now) {
      let Reverse(e) = guard.0.pop().expect("peeked");
      e.shared.done.store(true, Ordering::Release);
      if let Some(w) = e.shared.waker.lock().expect("sleep waker lock").take() {
        w.wake();
      }
    }
    guard = match guard.0.peek().map(|e| e.0.at) {
      Some(at) => {
        t.cv
          .wait_timeout(guard, at.saturating_duration_since(now))
          .expect("timer wait")
          .0
      }
      None => t.cv.wait(guard).expect("timer wait"),
    };
  }
}

struct Sleep {
  at: Instant,
  shared: Option<Arc<SleepShared>>,
}

impl Future for Sleep {
  type Output = ();

  fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
    match &self.shared {
      Some(s) => {
        if s.done.load(Ordering::Acquire) {
          return Poll::Ready(());
        }
        *s.waker.lock().expect("sleep waker lock") = Some(cx.waker().clone());
        if s.done.load(Ordering::Acquire) {
          Poll::Ready(())
        } else {
          Poll::Pending
        }
      }
      None => {
        if Instant::now() >= self.at {
          return Poll::Ready(());
        }
        let shared = Arc::new(SleepShared {
          done: AtomicBool::new(false),
          waker: Mutex::new(Some(cx.waker().clone())),
        });
        let t = timer();
        {
          let mut g = t.heap.lock().expect("timer lock");
          g.1 += 1;
          let seq = g.1;
          g.0.push(Reverse(TimerEntry {
            at: self.at,
            seq,
            shared: Arc::clone(&shared),
          }));
        }
        t.cv.notify_one();
        self.shared = Some(shared);
        Poll::Pending
      }
    }
  }
}

/// A sleep on the kernel's own timer thread.
pub fn timer_sleep(d: Duration) -> BoxFuture<'static, ()> {
  Box::pin(Sleep {
    at: Instant::now() + d,
    shared: None,
  })
}

// ── ParkExecutor ────────────────────────────────────────────────────────────────────────────

struct Task {
  fut: Mutex<Option<BoxFuture<'static, ()>>>,
  queued: AtomicBool,
  exec: Weak<ParkExecutor>,
}

impl Wake for Task {
  fn wake(self: Arc<Self>) {
    self.wake_by_ref();
  }

  fn wake_by_ref(self: &Arc<Self>) {
    if !self.queued.swap(true, Ordering::AcqRel)
      && let Some(ex) = self.exec.upgrade()
    {
      ex.queue
        .lock()
        .expect("park queue lock")
        .push_back(Arc::clone(self));
      ex.thread.unpark();
    }
  }
}

/// A single-threaded executor driven by the thread that called `block_on`; it parks when idle.
pub struct ParkExecutor {
  queue: Mutex<VecDeque<Arc<Task>>>,
  thread: Thread,
  me: Weak<ParkExecutor>,
}

impl Executor for ParkExecutor {
  fn spawn(&self, fiber: BoxFuture<'static, ()>) {
    let task = Arc::new(Task {
      fut: Mutex::new(Some(fiber)),
      queued: AtomicBool::new(true),
      exec: self.me.clone(),
    });
    self.queue.lock().expect("park queue lock").push_back(task);
    self.thread.unpark();
  }

  fn sleep(&self, d: Duration) -> BoxFuture<'static, ()> {
    timer_sleep(d)
  }
}

impl ParkExecutor {
  fn new() -> Arc<Self> {
    Arc::new_cyclic(|me| Self {
      queue: Mutex::new(VecDeque::new()),
      thread: thread::current(),
      me: me.clone(),
    })
  }

  fn run_until(&self, done: impl Fn() -> bool) {
    loop {
      loop {
        let next = self.queue.lock().expect("park queue lock").pop_front();
        let Some(task) = next else { break };
        task.queued.store(false, Ordering::Release);
        let waker = Waker::from(Arc::clone(&task));
        let mut cx = Context::from_waker(&waker);
        let mut slot = task.fut.lock().expect("task lock");
        if let Some(f) = slot.as_mut()
          && f.as_mut().poll(&mut cx).is_ready()
        {
          *slot = None;
        }
      }
      if done() {
        break;
      }
      thread::park();
    }
    // Break the task -> fiber -> executor cycle for anything still queued (daemons).
    self.queue.lock().expect("park queue lock").clear();
  }
}

fn assert_not_in_fiber(what: &str) {
  assert!(
    !fiber::in_fiber(),
    "kernel_b: `{what}` called inside a fiber; bind the effect with `~eff` instead \
     (this is the `request_resolver.rs:102` class of bug)"
  );
}

/// Run an env-free effect to completion on the calling thread, parking while idle.
/// Panics (with a diagnostic) when called from inside a fiber.
pub fn block_on<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E, ()>) -> Exit<A, E> {
  assert_not_in_fiber("block_on");
  if eff.is_ready() {
    // Ready fast path: no executor, no fiber.
    return pollster_ready(eff);
  }
  let exec = ParkExecutor::new();
  let handle: FiberHandle<A, E> = fiber::spawn_root(exec.clone(), eff, ());
  let state = Arc::clone(&handle.state);
  exec.run_until(|| state.is_done());
  handle.cell.take()
}

fn pollster_ready<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E, ()>) -> Exit<A, E> {
  match eff.step {
    crate::kernel::send::Step::Ready(v) => v,
    _ => unreachable!("checked by is_ready"),
  }
}

/// `run_blocking(eff, env)`: provide `env` and `block_on`.
pub fn run_blocking<A, E, R>(eff: Effect<A, E, R>, env: R) -> Exit<A, E>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + 'static,
{
  block_on(eff.provide(env))
}

impl<A: Send + 'static, E: Send + 'static> Effect<A, E, ()> {
  /// `block_on(self)`.
  pub fn block_on(self) -> Exit<A, E> {
    block_on(self)
  }
}

struct Unpark(Thread);

impl Wake for Unpark {
  fn wake(self: Arc<Self>) {
    self.0.unpark();
  }
}

/// Run a `LocalEffect` on the calling thread (no fibers; `fork` is not available locally).
pub fn block_on_local<A: 'static, E: 'static>(eff: LocalEffect<A, E, ()>) -> Exit<A, E> {
  assert_not_in_fiber("block_on_local");
  let waker = Waker::from(Arc::new(Unpark(thread::current())));
  let mut cx = Context::from_waker(&waker);
  let mut fut = eff.run_in(crate::kernel::unit_env());
  loop {
    if let Poll::Ready(v) = Pin::new(&mut fut).poll(&mut cx) {
      return v;
    }
    thread::park();
  }
}

impl<A: 'static, E: 'static> LocalEffect<A, E, ()> {
  pub fn block_on(self) -> Exit<A, E> {
    block_on_local(self)
  }
}

// ── tokio backend + Runtime ─────────────────────────────────────────────────────────────────

/// `tokio::spawn` with real wakers (replaces 0.6 `spawn_blocking` + `run_blocking`).
#[cfg(feature = "tokio")]
pub struct TokioExecutor {
  handle: tokio::runtime::Handle,
}

#[cfg(feature = "tokio")]
impl TokioExecutor {
  pub fn new(handle: tokio::runtime::Handle) -> Self {
    Self { handle }
  }
}

#[cfg(feature = "tokio")]
impl Executor for TokioExecutor {
  fn spawn(&self, fiber: BoxFuture<'static, ()>) {
    drop(self.handle.spawn(fiber));
  }

  fn sleep(&self, d: Duration) -> BoxFuture<'static, ()> {
    let handle = self.handle.clone();
    Box::pin(async move {
      let sleep = {
        let _ctx = handle.enter();
        tokio::time::sleep(d)
      };
      sleep.await;
    })
  }
}

/// Spawns root fibers on an executor.
#[derive(Clone)]
pub struct Runtime {
  exec: Arc<dyn Executor>,
}

impl Runtime {
  pub fn new(exec: Arc<dyn Executor>) -> Self {
    Self { exec }
  }

  /// A runtime over a tokio handle.
  #[cfg(feature = "tokio")]
  pub fn tokio(handle: tokio::runtime::Handle) -> Self {
    Self::new(Arc::new(TokioExecutor::new(handle)))
  }

  /// Spawn `eff` with `env` as a root fiber.
  pub fn spawn<A, E, R>(&self, eff: Effect<A, E, R>, env: R) -> FiberHandle<A, E>
  where
    A: Send + 'static,
    E: Send + 'static,
    R: Send + 'static,
  {
    fiber::spawn_root(Arc::clone(&self.exec), eff, env)
  }

  /// Run `eff` with `env` as a root fiber and await its exit. The future is `Send`.
  pub fn run<A, E, R>(
    &self,
    eff: Effect<A, E, R>,
    env: R,
  ) -> impl Future<Output = Exit<A, E>> + Send + 'static
  where
    A: Send + 'static,
    E: Send + 'static,
    R: Send + 'static,
  {
    self.spawn(eff, env).exit()
  }
}
