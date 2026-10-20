//! **Stratum 12 — Transactional Memory**
//!
//! Optimistic concurrency with composable transactions, built from Strata 0–11.
//!
//! Software transactional memory — optimistic transactions over [`TRef`] cells.
//!
//! A [`Stm<A, E>`] describes a transactional program. [`commit`] (alias [`atomically`]) lifts it
//! into a 0.7-kernel [`Effect`] that runs it to completion:
//!
//! - **Conflict:** when commit-time validation finds that a cell the attempt read was committed
//!   by someone else in between, the attempt restarts at once (a conflict means another
//!   transaction made progress).
//! - **Retry:** on [`Outcome::Retry`] (Effect.ts `STM.retry`) the fiber **parks** until one of
//!   the cells the attempt read or wrote changes. Each [`TRef`] keeps a wait list of the parked
//!   transactions' wakers; a commit that writes the cell wakes them. No thread is blocked and
//!   nothing spins. Interrupting a parked transaction removes its wakers from every cell.
//!   A transaction that retries without touching any cell (a bare [`Stm::retry`]) waits forever,
//!   until it is interrupted.
//! - **Fail:** a typed failure ends the commit with that error, unless the attempt saw an
//!   inconsistent snapshot (a cell it read has moved on), in which case it restarts.
//!
//! [`Stm::or_else`] rolls back the writes of a branch that retries before it runs the
//! alternative (Effect.ts `orTry` semantics); the reads of both branches stay in the wait set.
//!
//! Concurrency: all commits are serialized behind a global lock; transaction bodies record read
//! versions and defer writes until commit succeeds.
//!
//! Collections built on [`TRef`]: [`TQueue`], [`TMap`], [`TSemaphore`], [`TSet`], [`TArray`] (one
//! cell per element), [`TPriorityQueue`], [`TPromise`] and [`TReentrantLock`] (read/write lock,
//! reentrant per fiber: each attempt knows the id of the fiber that runs [`commit`]).

use std::any::Any;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::future::Future;
use std::hash::Hash;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::task::{Context, Poll, Waker};

use crate::{Effect, FiberId};

mod tarray;
mod tpriority_queue;
mod tpromise;
mod treentrant_lock;
mod tset;

pub use tarray::TArray;
pub use tpriority_queue::TPriorityQueue;
pub use tpromise::TPromise;
pub use treentrant_lock::TReentrantLock;
pub use tset::TSet;

// ── locking helpers ───────────────────────────────────────────────────────────

/// Locks `m`, recovering from poisoning (no user code runs while an STM lock is held, so a
/// poisoned lock only means a panic elsewhere on that thread).
fn lock<T: ?Sized>(m: &Mutex<T>) -> MutexGuard<'_, T> {
  m.lock().unwrap_or_else(PoisonError::into_inner)
}

static STM_COMMIT_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn commit_lock() -> &'static Mutex<()> {
  STM_COMMIT_LOCK.get_or_init(|| Mutex::new(()))
}

/// Ids for parked transactions (keys of the per-cell wait lists).
static NEXT_WAITER: AtomicU64 = AtomicU64::new(0);

// ── type-erased cells and shadow values ──────────────────────────────────────

/// A transaction-local copy of a cell value.
trait Shadow: Send {
  fn clone_box(&self) -> Box<dyn Shadow>;
  fn as_any(&self) -> &dyn Any;
  fn into_any(self: Box<Self>) -> Box<dyn Any>;
}

impl<T: Any + Clone + Send> Shadow for T {
  fn clone_box(&self) -> Box<dyn Shadow> {
    Box::new(self.clone())
  }
  fn as_any(&self) -> &dyn Any {
    self
  }
  fn into_any(self: Box<Self>) -> Box<dyn Any> {
    self
  }
}

/// The type-erased side of a [`TRef`] that commit and retry need.
trait TCell: Send + Sync {
  /// Current committed version.
  fn version(&self) -> u64;
  /// Stores `value` (a shadow of this cell's type), bumps the version and returns the wakers of
  /// every transaction parked on this cell. Called under the commit lock.
  fn apply(&self, value: Box<dyn Shadow>) -> Vec<Waker>;
  /// Parks waiter `id` on this cell, unless the cell has moved past `seen` (then `false`).
  fn watch(&self, id: u64, waker: &Waker, seen: u64) -> bool;
  /// Removes waiter `id` from this cell's wait list (no-op when absent).
  fn unwatch(&self, id: u64);
}

struct Inner<A> {
  ver: AtomicU64,
  data: Mutex<A>,
  /// Parked transactions (waiter id → waker), drained by the next commit that writes the cell.
  watchers: Mutex<HashMap<u64, Waker>>,
}

impl<A: Clone + Send + Sync + 'static> TCell for Inner<A> {
  fn version(&self) -> u64 {
    self.ver.load(Ordering::Acquire)
  }

  fn apply(&self, value: Box<dyn Shadow>) -> Vec<Waker> {
    let value = *value
      .into_any()
      .downcast::<A>()
      .unwrap_or_else(|_| unreachable!("stm: shadow type mismatch"));
    {
      let mut g = lock(&self.data);
      *g = value;
      self.ver.fetch_add(1, Ordering::Release);
    }
    // After the version bump: a waiter that registers after this drain sees the new version
    // (its check runs under the same `watchers` lock), so no wake-up is lost.
    lock(&self.watchers).drain().map(|(_, w)| w).collect()
  }

  fn watch(&self, id: u64, waker: &Waker, seen: u64) -> bool {
    let mut ws = lock(&self.watchers);
    if self.version() != seen {
      return false;
    }
    match ws.entry(id) {
      Entry::Occupied(mut e) => {
        if !e.get().will_wake(waker) {
          e.insert(waker.clone());
        }
      }
      Entry::Vacant(e) => {
        e.insert(waker.clone());
      }
    }
    true
  }

  fn unwatch(&self, id: u64) {
    lock(&self.watchers).remove(&id);
  }
}

// ── transaction log ────────────────────────────────────────────────────────

/// One attempt of a transactional read/write set.
pub struct Txn {
  /// `TRef` identity → this attempt's view of the cell (read snapshot or pending write).
  shadow: HashMap<usize, Box<dyn Shadow>>,
  /// `TRef` identity → (cell, version observed on first access). Validated at commit and
  /// watched on retry.
  reads: HashMap<usize, (Arc<dyn TCell>, u64)>,
  /// `TRef` identity → cell, for every cell written in this attempt.
  writes: HashMap<usize, Arc<dyn TCell>>,
  /// The fiber running [`commit`] (the owner identity of [`TReentrantLock`]).
  fiber: FiberId,
}

/// The rollback point of [`Stm::or_else`].
struct Checkpoint {
  shadow: HashMap<usize, Box<dyn Shadow>>,
  writes: HashMap<usize, Arc<dyn TCell>>,
}

impl Txn {
  fn new(fiber: FiberId) -> Self {
    Self {
      shadow: HashMap::new(),
      reads: HashMap::new(),
      writes: HashMap::new(),
      fiber,
    }
  }

  fn checkpoint(&self) -> Checkpoint {
    Checkpoint {
      shadow: self
        .shadow
        .iter()
        .map(|(id, v)| (*id, v.clone_box()))
        .collect(),
      writes: self.writes.clone(),
    }
  }

  /// Drops the writes made since `cp`; reads are kept so a later retry also watches them.
  fn rollback(&mut self, cp: Checkpoint) {
    self.shadow = cp.shadow;
    self.writes = cp.writes;
  }

  /// `true` while no cell this attempt observed has been committed by someone else.
  fn is_consistent(&self) -> bool {
    self.reads.values().all(|(c, ver)| c.version() == *ver)
  }

  /// The future a retrying attempt parks on.
  fn into_wait(self) -> RetryWait {
    RetryWait {
      id: NEXT_WAITER.fetch_add(1, Ordering::Relaxed),
      cells: self.reads.into_values().collect(),
    }
  }
}

/// Validates `txn` and applies its writes atomically. `false` on a conflict.
fn try_commit(mut txn: Txn) -> bool {
  let wakers = {
    let _guard = lock(commit_lock());
    if !txn.is_consistent() {
      return false;
    }
    let mut ids: Vec<usize> = txn.writes.keys().copied().collect();
    ids.sort_unstable();
    let mut wakers = Vec::new();
    for id in ids {
      let cell = txn
        .writes
        .remove(&id)
        .expect("stm: writes must contain sorted ids");
      let value = txn
        .shadow
        .remove(&id)
        .expect("stm: a written cell has a shadow value");
      wakers.extend(cell.apply(value));
    }
    wakers
  };
  for w in wakers {
    w.wake();
  }
  true
}

/// Parks a retrying transaction until a cell it observed changes. Dropping it (completion or
/// interruption) removes its waker from every cell.
struct RetryWait {
  id: u64,
  cells: Vec<(Arc<dyn TCell>, u64)>,
}

impl Future for RetryWait {
  type Output = ();

  fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
    let this = self.get_mut();
    for (cell, seen) in &this.cells {
      if !cell.watch(this.id, cx.waker(), *seen) {
        return Poll::Ready(());
      }
    }
    Poll::Pending
  }
}

impl Drop for RetryWait {
  fn drop(&mut self) {
    for (cell, _) in &self.cells {
      cell.unwatch(self.id);
    }
  }
}

// ── STM outcome / program ───────────────────────────────────────────────────

/// Result of one [`Stm`] execution pass over a [`Txn`] (no I/O yet).
pub enum Outcome<A, E> {
  /// Transactional step finished with value `A` (commit may still fail validation).
  Done(A),
  /// Transactional failure; no commit attempted.
  Fail(E),
  /// Park until a cell this attempt touched changes, then restart (Effect.ts `STM.retry`).
  Retry,
}

type StmRun<A, E> = dyn Fn(&mut Txn) -> Outcome<A, E> + Send + Sync;

/// Transactional program composable with [`Stm::flat_map`], [`Stm::map`], etc.
pub struct Stm<A, E>
where
  A: Send + 'static,
  E: Send + 'static,
{
  run: Arc<StmRun<A, E>>,
}

impl<A, E> Clone for Stm<A, E>
where
  A: Send + 'static,
  E: Send + 'static,
{
  fn clone(&self) -> Self {
    Self {
      run: Arc::clone(&self.run),
    }
  }
}

impl<A, E> Stm<A, E>
where
  A: Send + 'static,
  E: Send + 'static,
{
  /// Lift a pure success value (re-executed on each attempt; `A` should be cheap or [`Clone`]).
  pub fn succeed(a: A) -> Self
  where
    A: Clone + Send + Sync + 'static,
  {
    Self::from_fn(move |_| Outcome::Done(a.clone()))
  }

  /// Transactional failure (no commit attempted).
  pub fn fail(e: E) -> Self
  where
    E: Clone + Send + Sync + 'static,
  {
    Self::from_fn(move |_| Outcome::Fail(e.clone()))
  }

  /// Always retry: park until a cell read so far changes (e.g. blocking queue take).
  pub fn retry() -> Self {
    Self::from_fn(|_| Outcome::Retry)
  }

  /// Retry unless `cond` holds.
  pub fn check(cond: bool) -> Stm<(), E> {
    if cond { Stm::succeed(()) } else { Stm::retry() }
  }

  /// Lifts a raw transactional closure into an [`Stm`] program.
  pub fn from_fn(f: impl Fn(&mut Txn) -> Outcome<A, E> + Send + Sync + 'static) -> Self {
    Self { run: Arc::new(f) }
  }

  /// Maps a successful result without changing the error channel.
  pub fn map<B, F>(self, f: F) -> Stm<B, E>
  where
    B: Send + 'static,
    F: Fn(A) -> B + Send + Sync + 'static,
  {
    let left = self.run.clone();
    Stm::<B, E>::from_fn(move |txn| match left(txn) {
      Outcome::Done(a) => Outcome::Done(f(a)),
      Outcome::Fail(e) => Outcome::Fail(e),
      Outcome::Retry => Outcome::Retry,
    })
  }

  /// Monadic bind on success; errors and retries propagate.
  pub fn flat_map<B, F>(self, f: F) -> Stm<B, E>
  where
    B: Send + 'static,
    F: Fn(A) -> Stm<B, E> + Send + Sync + 'static,
  {
    let left = self.run.clone();
    Stm::<B, E>::from_fn(move |txn| match left(txn) {
      Outcome::Done(a) => f(a).run_on(txn),
      Outcome::Fail(e) => Outcome::Fail(e),
      Outcome::Retry => Outcome::Retry,
    })
  }

  /// On retry from `self`, roll back `self`'s writes and run `that`. Failures propagate.
  pub fn or_else(self, that: Stm<A, E>) -> Stm<A, E> {
    let r1 = self.run.clone();
    let r2 = that.run.clone();
    Self::from_fn(move |txn| {
      let cp = txn.checkpoint();
      match r1(txn) {
        Outcome::Retry => {
          txn.rollback(cp);
          r2(txn)
        }
        o => o,
      }
    })
  }

  /// Runs this program against an in-flight [`Txn`].
  pub fn run_on(&self, txn: &mut Txn) -> Outcome<A, E> {
    (self.run)(txn)
  }
}

// ── TRef ────────────────────────────────────────────────────────────────────

/// Transactional mutable cell (shared with [`Arc`] identity).
#[derive(Clone)]
pub struct TRef<A> {
  inner: Arc<Inner<A>>,
}

impl<A> TRef<A>
where
  A: Clone + Send + Sync + 'static,
{
  fn id(&self) -> usize {
    Arc::as_ptr(&self.inner) as usize
  }

  fn erased(&self) -> Arc<dyn TCell> {
    self.inner.clone()
  }

  /// Records the cell's current version in the read set on first access.
  fn track(&self, txn: &mut Txn) {
    if let Entry::Vacant(e) = txn.reads.entry(self.id()) {
      let ver = self.inner.version();
      e.insert((self.erased(), ver));
    }
  }

  /// A fresh cell holding `initial` (outside any transaction).
  fn new(initial: A) -> Self {
    TRef {
      inner: Arc::new(Inner {
        ver: AtomicU64::new(0),
        data: Mutex::new(initial),
        watchers: Mutex::new(HashMap::new()),
      }),
    }
  }

  /// Allocate a new cell (each retry creates a fresh cell unless hoisted outside `commit`).
  pub fn make(initial: A) -> Stm<TRef<A>, ()>
  where
    A: Clone,
  {
    Stm::from_fn(move |_| Outcome::Done(TRef::new(initial.clone())))
  }

  /// Read the current transactional or live value.
  pub fn read_stm<E: Send + 'static>(&self) -> Stm<A, E> {
    let r = self.clone();
    Stm::from_fn(move |txn| Outcome::Done(r.read_txn(txn)))
  }

  /// Writes `value` at commit time after recording validation for this cell.
  pub fn write_stm<E: Send + 'static>(&self, value: A) -> Stm<(), E> {
    let r = self.clone();
    Stm::from_fn(move |txn| {
      r.set_txn(txn, value.clone());
      Outcome::Done(())
    })
  }

  /// Reads, applies `f` to the current value, then writes the result back.
  pub fn update_stm<E, F>(&self, f: F) -> Stm<(), E>
  where
    F: Fn(A) -> A + Send + Sync + Clone + 'static,
    E: Send + 'static,
  {
    let r = self.clone();
    Stm::from_fn(move |txn| {
      let cur = r.read_txn(txn);
      r.set_txn(txn, f(cur));
      Outcome::Done(())
    })
  }

  /// Reads, computes `(output, next)` with `f`, writes `next`, and returns `output`.
  pub fn modify_stm<B, E, F>(&self, f: F) -> Stm<B, E>
  where
    B: Send + 'static,
    F: Fn(A) -> (B, A) + Send + Sync + Clone + 'static,
    E: Send + 'static,
  {
    let r = self.clone();
    Stm::from_fn(move |txn| {
      let cur = r.read_txn(txn);
      let (out, next) = f(cur);
      r.set_txn(txn, next);
      Outcome::Done(out)
    })
  }

  fn read_txn(&self, txn: &mut Txn) -> A {
    let id = self.id();
    if let Some(v) = txn.shadow.get(&id) {
      return v
        .as_any()
        .downcast_ref::<A>()
        .expect("stm: shadow type mismatch")
        .clone();
    }
    let (val, ver) = {
      let g = lock(&self.inner.data);
      (g.clone(), self.inner.version())
    };
    // Keep the first observed version: after an `or_else` rollback a re-read must still be
    // validated against what the attempt saw first.
    txn.reads.entry(id).or_insert_with(|| (self.erased(), ver));
    // Cache the snapshot so later reads in this attempt do not observe a newer live value.
    txn.shadow.insert(id, Box::new(val.clone()));
    val
  }

  fn set_txn(&self, txn: &mut Txn, value: A) {
    self.track(txn);
    let id = self.id();
    txn.shadow.insert(id, Box::new(value));
    txn.writes.insert(id, self.erased());
  }

  /// Number of transactions parked on this cell (test hook).
  #[cfg(test)]
  fn watcher_count(&self) -> usize {
    lock(&self.inner.watchers).len()
  }
}

// ── TQueue ─────────────────────────────────────────────────────────────────

#[derive(Clone, Default)]
struct QueueState<A> {
  buf: std::collections::VecDeque<A>,
  cap: Option<usize>,
}

/// Transactional queue backed by a [`TRef`].
#[derive(Clone)]
pub struct TQueue<A> {
  inner: TRef<QueueState<A>>,
}

impl<A> TQueue<A>
where
  A: Clone + Send + Sync + 'static,
{
  /// FIFO queue with at most `capacity` elements; [`TQueue::offer`] returns `false` when full.
  pub fn bounded(capacity: usize) -> Stm<TQueue<A>, ()> {
    TRef::make(QueueState {
      buf: std::collections::VecDeque::new(),
      cap: Some(capacity),
    })
    .map(|inner| TQueue { inner })
  }

  /// FIFO queue with no capacity limit.
  pub fn unbounded() -> Stm<TQueue<A>, ()> {
    TRef::make(QueueState {
      buf: std::collections::VecDeque::new(),
      cap: None,
    })
    .map(|inner| TQueue { inner })
  }

  /// `true` if the element was enqueued, `false` if bounded and full.
  pub fn offer<E>(&self, value: A) -> Stm<bool, E>
  where
    E: Send + 'static,
  {
    let q = self.clone();
    Stm::from_fn(move |txn| {
      let mut st = q.inner.read_txn(txn);
      if let Some(cap) = st.cap
        && st.buf.len() >= cap
      {
        return Outcome::Done(false);
      }
      st.buf.push_back(value.clone());
      q.inner.set_txn(txn, st);
      Outcome::Done(true)
    })
  }

  /// Remove the head element; [`Outcome::Retry`] (park until an offer) while empty.
  pub fn take<E>(&self) -> Stm<A, E>
  where
    E: Send + 'static,
  {
    let q = self.clone();
    Stm::from_fn(move |txn| {
      let mut st = q.inner.read_txn(txn);
      if let Some(x) = st.buf.pop_front() {
        q.inner.set_txn(txn, st);
        Outcome::Done(x)
      } else {
        Outcome::Retry
      }
    })
  }
}

// ── TMap ────────────────────────────────────────────────────────────────────

/// Transactional map in a [`TRef`] (hash map snapshot per transaction).
#[derive(Clone)]
pub struct TMap<K, V> {
  inner: TRef<HashMap<K, V>>,
}

impl<K, V> TMap<K, V>
where
  K: Clone + Eq + Hash + Send + Sync + 'static,
  V: Clone + Send + Sync + 'static,
{
  /// Empty transactional hash map.
  pub fn make() -> Stm<TMap<K, V>, ()> {
    TRef::make(HashMap::new()).map(|inner| TMap { inner })
  }

  /// Cloned value for `key`, if any.
  pub fn get<E>(&self, key: &K) -> Stm<Option<V>, E>
  where
    E: Send + 'static,
  {
    let m = self.clone();
    let key = key.clone();
    Stm::from_fn(move |txn| {
      let map = m.inner.read_txn(txn);
      Outcome::Done(map.get(&key).cloned())
    })
  }

  /// Inserts or replaces `key` → `value`.
  pub fn set<E>(&self, key: K, value: V) -> Stm<(), E>
  where
    E: Send + 'static,
  {
    let m = self.clone();
    Stm::from_fn(move |txn| {
      let mut map = m.inner.read_txn(txn);
      map.insert(key.clone(), value.clone());
      m.inner.set_txn(txn, map);
      Outcome::Done(())
    })
  }

  /// Removes `key` if present.
  pub fn delete<E>(&self, key: &K) -> Stm<(), E>
  where
    E: Send + 'static,
  {
    let m = self.clone();
    let key = key.clone();
    Stm::from_fn(move |txn| {
      let mut map = m.inner.read_txn(txn);
      map.remove(&key);
      m.inner.set_txn(txn, map);
      Outcome::Done(())
    })
  }
}

// ── TSemaphore ─────────────────────────────────────────────────────────────

/// Transactional permit counter in a [`TRef`].
#[derive(Clone)]
pub struct TSemaphore {
  inner: TRef<usize>,
}

impl TSemaphore {
  /// Permit counter initialized to `permits` ([`TSemaphore::acquire`] parks while zero).
  pub fn make(permits: usize) -> Stm<TSemaphore, ()> {
    TRef::make(permits).map(|inner| TSemaphore { inner })
  }

  /// Decrements permits by one, or [`Outcome::Retry`] while count is zero.
  pub fn acquire<E>(&self) -> Stm<(), E>
  where
    E: Send + 'static,
  {
    let s = self.clone();
    Stm::from_fn(move |txn| {
      let n = s.inner.read_txn(txn);
      if n == 0 {
        return Outcome::Retry;
      }
      s.inner.set_txn(txn, n - 1);
      Outcome::Done(())
    })
  }

  /// Increments the permit count by one.
  pub fn release<E>(&self) -> Stm<(), E>
  where
    E: Send + 'static,
  {
    let s = self.clone();
    Stm::from_fn(move |txn| {
      let n = s.inner.read_txn(txn);
      s.inner.set_txn(txn, n + 1);
      Outcome::Done(())
    })
  }
}

// ── commit ─────────────────────────────────────────────────────────────────

/// Run `stm` until it commits successfully, as an async 0.7-kernel [`Effect`].
///
/// Conflicts restart the attempt; [`Outcome::Retry`] parks the fiber (no thread is blocked)
/// until a cell the attempt touched is committed by another transaction. The effect is
/// interruptible while parked.
pub fn commit<A, E, R>(stm: Stm<A, E>) -> Effect<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  Effect::from_ctx(move |ctx| {
    let fiber = ctx.fiber().id();
    async move {
      loop {
        let mut txn = Txn::new(fiber);
        match stm.run_on(&mut txn) {
          Outcome::Fail(e) => {
            if txn.is_consistent() {
              return Err(e);
            }
          }
          Outcome::Retry => txn.into_wait().await,
          Outcome::Done(a) => {
            if try_commit(txn) {
              return Ok(a);
            }
          }
        }
      }
    }
  })
}

/// Alias for [`commit`] (Effect.ts naming).
pub fn atomically<A, E, R>(stm: Stm<A, E>) -> Effect<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  commit(stm)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::{Cause, Exit, FiberHandle, HNil, TestExecutor};
  use std::thread;
  use std::time::Duration;

  /// Runs `eff` on the global runtime; typed failures become `Err`, anything else panics.
  fn run<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> Result<A, E> {
    match eff.block_on() {
      Exit::Success(a) => Ok(a),
      Exit::Failure(Cause::Fail(e)) => Err(e),
      Exit::Failure(_) => panic!("effect died or was interrupted"),
    }
  }

  fn run_stm<A: Send + 'static, E: Send + 'static>(stm: Stm<A, E>) -> Result<A, E> {
    run(commit(stm))
  }

  fn cell<A: Clone + Send + Sync + 'static>(a: A) -> TRef<A> {
    run_stm(TRef::make(a)).expect("cell")
  }

  /// Forks `stm`'s commit as a root fiber on `exec` and runs it until it stalls.
  fn spawn_on<A: Send + 'static>(exec: &TestExecutor, stm: Stm<A, ()>) -> FiberHandle<A, ()> {
    let h = commit(stm).fork_root(Arc::new(exec.clone()));
    exec.run_until_stalled();
    h
  }

  /// A transaction that parks while `c` is zero and returns its value otherwise.
  fn wait_nonzero(c: &TRef<u32>) -> Stm<u32, ()> {
    let c = c.clone();
    Stm::from_fn(move |txn| {
      let n = c.read_txn(txn);
      if n == 0 {
        Outcome::Retry
      } else {
        Outcome::Done(n)
      }
    })
  }

  mod stm_constructors {
    use super::*;

    #[test]
    fn succeed_returns_value_immediately() {
      assert_eq!(run_stm(Stm::<i32, ()>::succeed(42)), Ok(42));
    }

    #[test]
    fn fail_returns_error_immediately() {
      assert_eq!(run_stm(Stm::<i32, &str>::fail("boom")), Err("boom"));
    }

    #[test]
    fn check_true_returns_unit_success() {
      assert_eq!(run_stm(Stm::<(), ()>::check(true)), Ok(()));
    }
  }

  mod stm_functor {
    use super::*;

    #[test]
    fn map_transforms_success_value() {
      let stm = Stm::<i32, ()>::succeed(3).map(|n| n * 2);
      assert_eq!(run_stm(stm), Ok(6));
    }

    #[test]
    fn map_preserves_failure() {
      let stm = Stm::<i32, &str>::fail("err").map(|n| n + 1);
      assert_eq!(run_stm(stm), Err("err"));
    }
  }

  mod stm_monad {
    use super::*;

    #[test]
    fn flat_map_sequences_two_succeed_values() {
      let stm = Stm::<i32, ()>::succeed(5).flat_map(|n| Stm::succeed(n + 1));
      assert_eq!(run_stm(stm), Ok(6));
    }

    #[test]
    fn flat_map_propagates_first_failure() {
      let stm = Stm::<i32, &str>::fail("e").flat_map(|_| Stm::succeed(0));
      assert_eq!(run_stm(stm), Err("e"));
    }

    #[test]
    fn or_else_picks_second_when_first_retries() {
      let stm = Stm::<i32, ()>::retry().or_else(Stm::succeed(7));
      assert_eq!(run_stm(stm), Ok(7));
    }

    #[test]
    fn or_else_preserves_first_success() {
      let stm = Stm::<i32, ()>::succeed(1).or_else(Stm::succeed(99));
      assert_eq!(run_stm(stm), Ok(1));
    }

    #[test]
    fn or_else_propagates_failure_without_trying_second() {
      let stm = Stm::<i32, &str>::fail("e").or_else(Stm::succeed(0));
      assert_eq!(run_stm(stm), Err("e"));
    }

    #[test]
    fn or_else_rolls_back_the_writes_of_a_retrying_branch() {
      let c = cell(1i32);
      let (c1, c2) = (c.clone(), c.clone());
      let left = c
        .write_stm::<()>(100)
        .flat_map(|()| Stm::<i32, ()>::retry());
      let right = Stm::from_fn(move |txn| Outcome::Done(c1.read_txn(txn)));
      assert_eq!(
        run_stm(left.or_else(right)),
        Ok(1),
        "right sees the original value"
      );
      assert_eq!(
        run_stm(c2.read_stm::<()>()),
        Ok(1),
        "left's write is not committed"
      );
    }
  }

  mod tref {
    use super::*;

    #[test]
    fn make_creates_tref_with_initial_value() {
      let got = run_stm(TRef::make(10i32).flat_map(|r| r.read_stm::<()>()));
      assert_eq!(got, Ok(10));
    }

    #[test]
    fn write_then_read_returns_new_value() {
      let got = run_stm(TRef::make(0i32).flat_map(|r| {
        let r2 = r.clone();
        r.write_stm::<()>(42).flat_map(move |_| r2.read_stm::<()>())
      }));
      assert_eq!(got, Ok(42));
    }

    #[test]
    fn update_applies_function_to_value() {
      let got = run_stm(TRef::make(3i32).flat_map(|r| {
        let r2 = r.clone();
        r.update_stm::<(), _>(|n| n * 2)
          .flat_map(move |_| r2.read_stm::<()>())
      }));
      assert_eq!(got, Ok(6));
    }

    #[test]
    fn modify_applies_and_returns_derived_value() {
      let got = run_stm(TRef::make(5i32).flat_map(|r| r.modify_stm::<i32, (), _>(|n| (n + 1, n))));
      assert_eq!(got, Ok(6));
    }

    #[test]
    fn tref_update_visible_after_commit() {
      let c = cell(0i32);
      run_stm(c.write_stm::<()>(42)).expect("write");
      assert_eq!(run_stm(c.read_stm::<()>()), Ok(42));
    }

    #[test]
    fn failed_transaction_writes_nothing() {
      let c = cell(1i32);
      let stm = c
        .write_stm::<&str>(9)
        .flat_map(|()| Stm::<(), &str>::fail("no"));
      assert_eq!(run_stm(stm), Err("no"));
      assert_eq!(run_stm(c.read_stm::<()>()), Ok(1));
    }
  }

  mod retry_parks {
    use super::*;

    #[test]
    fn stm_retry_waits_for_tref_change_across_threads() {
      let c = cell(0u32);
      let c_w = c.clone();
      let waiter = thread::spawn(move || run_stm(wait_nonzero(&c_w)).expect("waiter"));
      thread::sleep(Duration::from_millis(20));
      run_stm(c.write_stm::<()>(1)).expect("set");
      assert_eq!(waiter.join().expect("join"), 1);
    }

    #[test]
    fn retry_parks_the_fiber_and_registers_one_waker() {
      let exec = TestExecutor::new();
      let c = cell(0u32);
      let h = spawn_on(&exec, wait_nonzero(&c));
      assert!(!h.is_done(), "parked, not spinning");
      assert_eq!(c.watcher_count(), 1);
      assert_eq!(exec.queued_tasks(), 0, "nothing is re-queued while parked");
    }

    #[test]
    fn commit_to_a_read_cell_wakes_the_parked_transaction() {
      let exec = TestExecutor::new();
      let c = cell(0u32);
      let h = spawn_on(&exec, wait_nonzero(&c));
      let w = spawn_on(&exec, c.write_stm::<()>(5));
      assert!(w.is_done());
      assert!(h.is_done(), "woken by the commit and re-run");
      assert_eq!(c.watcher_count(), 0);
      assert_eq!(run(h.join::<HNil>()), Ok(5));
    }

    #[test]
    fn commit_to_an_unrelated_cell_does_not_wake() {
      let exec = TestExecutor::new();
      let c = cell(0u32);
      let other = cell(0u32);
      let h = spawn_on(&exec, wait_nonzero(&c));
      spawn_on(&exec, other.write_stm::<()>(1));
      assert!(!h.is_done());
      assert_eq!(c.watcher_count(), 1);
      assert_eq!(other.watcher_count(), 0);
    }

    #[test]
    fn retry_watches_every_cell_read_by_both_or_else_branches() {
      let exec = TestExecutor::new();
      let a = cell(0u32);
      let b = cell(0u32);
      let h = spawn_on(&exec, wait_nonzero(&a).or_else(wait_nonzero(&b)));
      assert_eq!((a.watcher_count(), b.watcher_count()), (1, 1));
      spawn_on(&exec, b.write_stm::<()>(3));
      assert!(h.is_done());
      assert_eq!((a.watcher_count(), b.watcher_count()), (0, 0));
    }

    #[test]
    fn interrupting_a_parked_transaction_deregisters_it() {
      let exec = TestExecutor::new();
      let a = cell(0u32);
      let b = cell(0u32);
      let h = spawn_on(&exec, wait_nonzero(&a).or_else(wait_nonzero(&b)));
      assert_eq!((a.watcher_count(), b.watcher_count()), (1, 1));
      h.interrupt_now();
      exec.run_until_stalled();
      assert!(h.is_done());
      assert_eq!((a.watcher_count(), b.watcher_count()), (0, 0));
      // A later commit finds no stale waker.
      spawn_on(&exec, a.write_stm::<()>(1));
      assert_eq!(a.watcher_count(), 0);
    }

    #[test]
    fn a_change_between_read_and_park_is_not_lost() {
      // The attempt reads 0, then another commit lands before it parks: the version check in
      // `watch` sees the change and the attempt restarts instead of sleeping.
      let c = cell(0u32);
      let mut txn = Txn::new(FiberId::ROOT);
      assert_eq!(c.read_txn(&mut txn), 0);
      run_stm(c.write_stm::<()>(1)).expect("write");
      let mut wait = std::pin::pin!(txn.into_wait());
      let mut cx = Context::from_waker(Waker::noop());
      assert!(wait.as_mut().poll(&mut cx).is_ready());
    }
  }

  mod tmap {
    use super::*;

    #[test]
    fn set_then_get_returns_inserted_value() {
      let got = run_stm(TMap::<&str, i32>::make().flat_map(|m| {
        let m2 = m.clone();
        m.set::<()>("k", 42).flat_map(move |_| m2.get::<()>(&"k"))
      }));
      assert_eq!(got, Ok(Some(42)));
    }

    #[test]
    fn get_absent_key_returns_none() {
      let got = run_stm(TMap::<&str, i32>::make().flat_map(|m| m.get::<()>(&"missing")));
      assert_eq!(got, Ok(None));
    }

    #[test]
    fn delete_removes_key() {
      let got = run_stm(TMap::<&str, i32>::make().flat_map(|m| {
        let m2 = m.clone();
        let m3 = m.clone();
        m.set::<()>("k", 1)
          .flat_map(move |_| m2.delete::<()>(&"k"))
          .flat_map(move |_| m3.get::<()>(&"k"))
      }));
      assert_eq!(got, Ok(None));
    }
  }

  mod tqueue {
    use super::*;

    #[test]
    fn offer_then_take_returns_value() {
      let got = run_stm(TQueue::<i32>::unbounded().flat_map(|q| {
        let q2 = q.clone();
        q.offer::<()>(99).flat_map(move |_| q2.take::<()>())
      }));
      assert_eq!(got, Ok(99));
    }

    #[test]
    fn tqueue_take_parks_until_offer() {
      let exec = TestExecutor::new();
      let q: TQueue<i32> = run_stm(TQueue::unbounded()).expect("make queue");
      let take = spawn_on(&exec, q.take::<()>());
      assert!(!take.is_done());
      spawn_on(&exec, q.offer::<()>(7));
      assert!(take.is_done());
      assert_eq!(run(take.join::<HNil>()), Ok(7));
    }

    #[test]
    fn bounded_queue_offer_returns_true_when_capacity_available() {
      let got = run_stm(TQueue::<i32>::bounded(2).flat_map(|q| q.offer::<()>(1)));
      assert_eq!(got, Ok(true));
    }
  }

  mod tsemaphore {
    use super::*;

    #[test]
    fn acquire_then_release_increments_permits_back() {
      let got = run_stm(TSemaphore::make(1).flat_map(|s| {
        let s2 = s.clone();
        s.acquire::<()>().flat_map(move |_| s2.release::<()>())
      }));
      assert_eq!(got, Ok(()));
    }

    #[test]
    fn tsemaphore_acquire_blocks_when_zero() {
      let sem = run_stm(TSemaphore::make(0usize)).expect("make sem");
      let sem_c = sem.clone();
      let acq = thread::spawn(move || {
        run_stm(sem_c.acquire::<()>()).expect("acquire");
      });
      thread::sleep(Duration::from_millis(20));
      run_stm(sem.release::<()>()).expect("release");
      acq.join().expect("join");
    }
  }

  mod atomically {
    use super::*;

    #[test]
    fn atomically_is_alias_for_commit() {
      assert_eq!(run(atomically(Stm::<i32, ()>::succeed(5))), Ok(5));
    }
  }
}
