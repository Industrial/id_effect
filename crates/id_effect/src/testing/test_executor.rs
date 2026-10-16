//! `TestExecutor`: a single-threaded, deterministic `Executor` with a virtual clock.
//!
//! Safe Rust only. The run queue is a `Mutex<VecDeque<Arc<Task>>>` and the timers are a
//! `BinaryHeap` keyed on `(deadline, seq)`. `!Send` futures from `spawn_local` live in a
//! thread-local table on the owner thread, so a task's waker stays `Send + Sync`.
//!
//! A seeded executor ([`TestExecutor::seeded`]) picks the next queued task with a SplitMix64
//! generator instead of FIFO, so one seed is one reproducible interleaving of the ready fibers;
//! [`explore_interleavings`] runs a test under many seeds and reports the first failing one.

use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, VecDeque};
use std::fmt;
use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, ThreadId};
use std::time::{Duration, Instant};

use crate::random::splitmix64;
use crate::runtime::{BoxFuture, Executor, LocalBoxFuture, NotLocal};

static NEXT_EXECUTOR_ID: AtomicU64 = AtomicU64::new(0);

thread_local! {
  /// `spawn_local` futures, keyed by `(executor id, task id)`, on the executor's owner thread.
  static LOCAL_TASKS: RefCell<HashMap<(u64, u64), LocalBoxFuture<'static, ()>>> =
    RefCell::new(HashMap::new());
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
  m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A single-threaded, deterministic [`Executor`] with a virtual clock, for tests.
///
/// Nothing runs until the test drives it:
///
/// - [`run_until_stalled`](Self::run_until_stalled) polls queued tasks in FIFO order until none
///   is queued. A task is queued when it is spawned and each time its waker is woken (from any
///   thread); a task that is not woken is not polled again, so nothing busy-polls.
/// - [`advance`](Self::advance) moves the virtual clock forward, firing due sleeps in
///   `(deadline, registration order)` order and running the woken tasks at each deadline.
/// - [`now`](Executor::now) is the virtual clock. It starts at the wall-clock instant of
///   [`new`](Self::new) and moves only through `advance`.
///
/// [`seeded`](Self::seeded) builds an executor that explores interleavings: instead of FIFO,
/// each poll picks one of the queued tasks with a generator seeded by `seed`. Every fiber is one
/// task, so the seed selects the order in which ready fibers run (and how their steps between
/// suspension points, such as [`yield_fiber`](crate::yield_fiber), interleave). Single-threaded
/// runs with the same seed and the same spawns make the same choices, so a failure found under a
/// seed reproduces by re-running with it; [`explore_interleavings`] runs a test under many seeds.
///
/// `spawn` accepts futures from any thread. `spawn_local` succeeds only on the thread that
/// created the executor (its owner thread), which is also where local tasks are polled.
/// `spawn_blocking` queues the closure as an ordinary task, so it runs inside
/// `run_until_stalled` on the driving thread.
///
/// Cloning gives another handle to the same executor.
#[derive(Clone)]
pub struct TestExecutor {
  inner: Arc<Inner>,
}

struct Inner {
  id: u64,
  owner: ThreadId,
  next_task: AtomicU64,
  queue: Mutex<VecDeque<Arc<Task>>>,
  clock: Mutex<Clock>,
  /// `Some((seed, generator state))` for a seeded executor; `None` polls in FIFO order.
  order: Option<(u64, Mutex<u64>)>,
}

struct Clock {
  now: Instant,
  next_seq: u64,
  timers: BinaryHeap<TimerEntry>,
}

enum Body {
  Send(Mutex<Option<BoxFuture<'static, ()>>>),
  /// The future is in `LOCAL_TASKS` on the owner thread.
  Local,
}

struct Task {
  id: u64,
  body: Body,
  queued: AtomicBool,
  exec: Weak<Inner>,
}

impl Wake for Task {
  fn wake(self: Arc<Self>) {
    self.wake_by_ref();
  }

  fn wake_by_ref(self: &Arc<Self>) {
    if !self.queued.swap(true, AtomicOrdering::AcqRel)
      && let Some(inner) = self.exec.upgrade()
    {
      lock(&inner.queue).push_back(Arc::clone(self));
    }
  }
}

struct Timer {
  fired: AtomicBool,
  waker: Mutex<Option<Waker>>,
}

impl Timer {
  fn fire(&self) {
    self.fired.store(true, AtomicOrdering::Release);
    if let Some(w) = lock(&self.waker).take() {
      w.wake();
    }
  }
}

struct TimerEntry {
  deadline: Instant,
  seq: u64,
  timer: Arc<Timer>,
}

impl PartialEq for TimerEntry {
  fn eq(&self, other: &Self) -> bool {
    (self.deadline, self.seq) == (other.deadline, other.seq)
  }
}

impl Eq for TimerEntry {}

impl PartialOrd for TimerEntry {
  fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
    Some(self.cmp(other))
  }
}

impl Ord for TimerEntry {
  /// Reversed, so the `BinaryHeap` (a max-heap) yields the earliest `(deadline, seq)` first.
  fn cmp(&self, other: &Self) -> Ordering {
    (other.deadline, other.seq).cmp(&(self.deadline, self.seq))
  }
}

/// The future returned by [`TestExecutor`]'s `sleep`: completes once `advance` reaches its
/// deadline.
struct Sleep {
  timer: Arc<Timer>,
}

impl Future for Sleep {
  type Output = ();

  fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
    if self.timer.fired.load(AtomicOrdering::Acquire) {
      return Poll::Ready(());
    }
    *lock(&self.timer.waker) = Some(cx.waker().clone());
    // Re-check: the timer may have fired between the first check and storing the waker.
    if self.timer.fired.load(AtomicOrdering::Acquire) {
      Poll::Ready(())
    } else {
      Poll::Pending
    }
  }
}

impl TestExecutor {
  /// A new executor owned by the calling thread, with the virtual clock at the current instant.
  /// Queued tasks run in FIFO order.
  pub fn new() -> Self {
    Self::with_order(None)
  }

  /// Like [`new`](Self::new), but each poll picks one of the queued tasks with a SplitMix64
  /// generator seeded by `seed` instead of the front one: the seed selects the interleaving of
  /// the ready fibers. The same seed, driven the same way from one thread, makes the same choices.
  pub fn seeded(seed: u64) -> Self {
    Self::with_order(Some((seed, Mutex::new(seed))))
  }

  /// The seed of a [`seeded`](Self::seeded) executor; `None` for a FIFO one.
  pub fn seed(&self) -> Option<u64> {
    self.inner.order.as_ref().map(|(seed, _)| *seed)
  }

  fn with_order(order: Option<(u64, Mutex<u64>)>) -> Self {
    Self {
      inner: Arc::new(Inner {
        id: NEXT_EXECUTOR_ID.fetch_add(1, AtomicOrdering::Relaxed),
        owner: thread::current().id(),
        next_task: AtomicU64::new(0),
        queue: Mutex::new(VecDeque::new()),
        clock: Mutex::new(Clock {
          now: Instant::now(),
          next_seq: 0,
          timers: BinaryHeap::new(),
        }),
        order,
      }),
    }
  }

  /// Polls queued tasks in FIFO order (in seeded order for [`seeded`](Self::seeded)) until no
  /// task is queued, and returns how many polls ran.
  ///
  /// Tasks woken while this runs are queued at the back and polled in the same call. Returns
  /// when every remaining task is waiting on a waker (a sleep, or a wake from elsewhere).
  ///
  /// # Panics
  ///
  /// If a task panics (the panic propagates), or if a `spawn_local` task is reached on a thread
  /// other than the owner thread.
  pub fn run_until_stalled(&self) -> usize {
    let mut polls = 0;
    loop {
      let Some(task) = self.next_queued() else {
        return polls;
      };
      // Cleared before polling, so a wake during the poll re-queues the task.
      task.queued.store(false, AtomicOrdering::Release);
      if self.poll_task(&task) {
        polls += 1;
      }
    }
  }

  /// Moves the virtual clock forward by `d`, firing sleeps as their deadlines are reached.
  ///
  /// First runs queued tasks. Then, for each distinct deadline `≤ now + d` in order: sets the
  /// clock to that deadline, fires every sleep due then (in registration order) and runs the
  /// woken tasks until stalled. Sleeps registered on the way are included if they fall due
  /// within `d`. Finally the clock is set to `now + d`.
  pub fn advance(&self, d: Duration) {
    let target = lock(&self.inner.clock).now + d;
    self.run_until_stalled();
    loop {
      let due = {
        let mut clock = lock(&self.inner.clock);
        match clock.timers.peek().map(|t| t.deadline) {
          Some(deadline) if deadline <= target => {
            if deadline > clock.now {
              clock.now = deadline;
            }
            let mut due = Vec::new();
            while clock.timers.peek().is_some_and(|t| t.deadline == deadline) {
              if let Some(entry) = clock.timers.pop() {
                due.push(entry.timer);
              }
            }
            due
          }
          _ => {
            clock.now = target;
            break;
          }
        }
      };
      // Fired without the clock lock held, so woken tasks may register new sleeps.
      for timer in due {
        timer.fire();
      }
      self.run_until_stalled();
    }
    self.run_until_stalled();
  }

  /// The number of sleeps registered and not yet fired (including ones whose future was
  /// dropped).
  pub fn pending_timers(&self) -> usize {
    lock(&self.inner.clock).timers.len()
  }

  /// The earliest deadline among pending sleeps, if any.
  pub fn next_timer_deadline(&self) -> Option<Instant> {
    lock(&self.inner.clock).timers.peek().map(|t| t.deadline)
  }

  /// The number of tasks queued to be polled.
  pub fn queued_tasks(&self) -> usize {
    lock(&self.inner.queue).len()
  }

  /// Dequeues the next task: the front one, or a seeded pick among the queued ones.
  fn next_queued(&self) -> Option<Arc<Task>> {
    let mut queue = lock(&self.inner.queue);
    match &self.inner.order {
      None => queue.pop_front(),
      Some(_) if queue.is_empty() => None,
      Some((_, state)) => {
        let pick = splitmix64(&mut lock(state)) % queue.len() as u64;
        queue.remove(pick as usize)
      }
    }
  }

  fn next_task_id(&self) -> u64 {
    self.inner.next_task.fetch_add(1, AtomicOrdering::Relaxed)
  }

  fn enqueue(&self, id: u64, body: Body) {
    let task = Arc::new(Task {
      id,
      body,
      queued: AtomicBool::new(true),
      exec: Arc::downgrade(&self.inner),
    });
    lock(&self.inner.queue).push_back(task);
  }

  /// Polls `task` once if its future is still live; returns whether a poll ran.
  fn poll_task(&self, task: &Arc<Task>) -> bool {
    let waker = Waker::from(Arc::clone(task));
    let mut cx = Context::from_waker(&waker);
    match &task.body {
      Body::Send(slot) => {
        // Taken out of the slot so the poll runs without the lock held.
        let Some(mut fut) = lock(slot).take() else {
          return false;
        };
        if fut.as_mut().poll(&mut cx).is_pending() {
          *lock(slot) = Some(fut);
        }
        true
      }
      Body::Local => {
        assert!(
          thread::current().id() == self.inner.owner,
          "TestExecutor: a spawn_local task can only be polled on the executor's owner thread"
        );
        let key = (self.inner.id, task.id);
        // Removed from the table so the poll may itself call `spawn_local`.
        let Some(mut fut) = LOCAL_TASKS.with(|m| m.borrow_mut().remove(&key)) else {
          return false;
        };
        if fut.as_mut().poll(&mut cx).is_pending() {
          LOCAL_TASKS.with(|m| m.borrow_mut().insert(key, fut));
        }
        true
      }
    }
  }
}

impl Default for TestExecutor {
  fn default() -> Self {
    Self::new()
  }
}

impl fmt::Debug for TestExecutor {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("TestExecutor")
      .field("id", &self.inner.id)
      .field("seed", &self.seed())
      .field("queued_tasks", &self.queued_tasks())
      .field("pending_timers", &self.pending_timers())
      .finish()
  }
}

impl Drop for Inner {
  fn drop(&mut self) {
    if thread::current().id() == self.owner {
      let id = self.id;
      // `try_with`: during thread teardown the table may already be gone.
      let _ = LOCAL_TASKS.try_with(|m| {
        if let Ok(mut m) = m.try_borrow_mut() {
          m.retain(|&(exec, _), _| exec != id);
        }
      });
    }
  }
}

impl Executor for TestExecutor {
  fn spawn(&self, fut: BoxFuture<'static, ()>) {
    self.enqueue(self.next_task_id(), Body::Send(Mutex::new(Some(fut))));
  }

  /// `Ok` on the owner thread; `Err(NotLocal)` elsewhere.
  fn spawn_local(&self, fut: LocalBoxFuture<'static, ()>) -> Result<(), NotLocal> {
    if thread::current().id() != self.inner.owner {
      return Err(NotLocal);
    }
    let id = self.next_task_id();
    LOCAL_TASKS.with(|m| m.borrow_mut().insert((self.inner.id, id), fut));
    self.enqueue(id, Body::Local);
    Ok(())
  }

  /// Queued as an ordinary task: the closure runs during `run_until_stalled`.
  fn spawn_blocking(&self, f: Box<dyn FnOnce() + Send>) {
    self.spawn(Box::pin(async move { f() }));
  }

  /// Completes when [`advance`](TestExecutor::advance) reaches `now() + d`; at once if `d` is
  /// zero.
  fn sleep(&self, d: Duration) -> BoxFuture<'static, ()> {
    let timer = Arc::new(Timer {
      fired: AtomicBool::new(d.is_zero()),
      waker: Mutex::new(None),
    });
    if !d.is_zero() {
      let mut clock = lock(&self.inner.clock);
      let deadline = clock.now + d;
      let seq = clock.next_seq;
      clock.next_seq += 1;
      clock.timers.push(TimerEntry {
        deadline,
        seq,
        timer: Arc::clone(&timer),
      });
    }
    Box::pin(Sleep { timer })
  }

  fn now(&self) -> Instant {
    lock(&self.inner.clock).now
  }
}

/// The first seed under which an [`explore_interleavings`] test failed, and its panic message.
///
/// Its `Display` names the seed and how to reproduce the run with [`TestExecutor::seeded`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterleavingFailure {
  /// The seed of the failing run.
  pub seed: u64,
  /// The panic message of the failing run (`"<non-string panic payload>"` if it had none).
  pub message: String,
}

impl fmt::Display for InterleavingFailure {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(
      f,
      "interleaving test failed under seed {seed}: {msg}\nreproduce with TestExecutor::seeded({seed})",
      seed = self.seed,
      msg = self.message
    )
  }
}

impl std::error::Error for InterleavingFailure {}

/// Runs `test` once per seed on a fresh [`TestExecutor::seeded`] executor, in the order the seeds
/// are given, and returns the first seed under which it panicked.
///
/// `test` drives the executor itself (fork fibers onto it, `run_until_stalled`, `advance`) and
/// asserts on the outcome. Its panic is caught, so the remaining seeds are not run; the panic
/// hook still prints it as usual. Re-running the same body on `TestExecutor::seeded(seed)`
/// reproduces the failure.
///
/// ```
/// use std::sync::Arc;
/// use std::sync::atomic::{AtomicU64, Ordering};
/// use id_effect::{Effect, TestExecutor, explore_interleavings, succeed, sync, yield_fiber, zip_par};
///
/// // A planted lost update: each side reads, yields, then writes back what it read plus one.
/// // In FIFO order the second side (which yields first) reads after the first side wrote, so
/// // `TestExecutor::new()` never sees the race; some seeds interleave the two reads.
/// fn bump(n: &Arc<AtomicU64>, pause: bool) -> Effect<()> {
///   let (r, w) = (Arc::clone(n), Arc::clone(n));
///   let gap: Effect<()> = if pause { yield_fiber() } else { succeed(()) };
///   gap
///     .flat_map(move |()| sync(move || r.load(Ordering::SeqCst)))
///     .flat_map(move |v| yield_fiber().map(move |()| v))
///     .flat_map(move |v| sync(move || w.store(v + 1, Ordering::SeqCst)))
/// }
///
/// let run = |exec: &TestExecutor| {
///   let n = Arc::new(AtomicU64::new(0));
///   let h = zip_par(bump(&n, false), bump(&n, true)).fork_root(Arc::new(exec.clone()));
///   exec.run_until_stalled();
///   assert!(h.is_done());
///   assert_eq!(n.load(Ordering::SeqCst), 2, "lost update");
/// };
///
/// let failure = explore_interleavings(0..64, run).expect_err("some seed loses the update");
/// // Re-running with the reported seed reproduces the failure.
/// let again = explore_interleavings([failure.seed], run).unwrap_err();
/// assert_eq!(again, failure);
/// ```
pub fn explore_interleavings<I, F>(seeds: I, test: F) -> Result<(), InterleavingFailure>
where
  I: IntoIterator<Item = u64>,
  F: Fn(&TestExecutor),
{
  for seed in seeds {
    let exec = TestExecutor::seeded(seed);
    if let Err(payload) = catch_unwind(AssertUnwindSafe(|| test(&exec))) {
      let message = payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "<non-string panic payload>".to_owned());
      return Err(InterleavingFailure { seed, message });
    }
  }
  Ok(())
}

/// [`explore_interleavings`] that panics with the [`InterleavingFailure`] (the failing seed and
/// how to reproduce it) instead of returning it.
///
/// # Panics
///
/// If `test` panics under any of `seeds`.
#[track_caller]
pub fn assert_interleavings<I, F>(seeds: I, test: F)
where
  I: IntoIterator<Item = u64>,
  F: Fn(&TestExecutor),
{
  if let Err(failure) = explore_interleavings(seeds, test) {
    panic!("{failure}");
  }
}
