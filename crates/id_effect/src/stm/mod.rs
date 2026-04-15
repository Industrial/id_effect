//! **Stratum 12 — Transactional Memory**
//!
//! Optimistic concurrency with composable transactions, built from Strata 0–11.
//!
//! Software transactional memory — optimistic transactions over [`TRef`] cells.
//!
//! A [`Stm<A, E>`] describes a transactional program. [`commit`] (alias
//! [`atomically`]) runs it to completion: on [`Outcome::Retry`] or a
//! failed commit validation the attempt restarts after [`std::thread::yield_now`].
//!
//! Concurrency: all commits are serialized behind a global lock; transaction bodies
//! record read versions and defer writes until commit succeeds.

use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::kernel::Effect;

// ── global commit serialization ─────────────────────────────────────────────

static STM_COMMIT_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn commit_lock() -> &'static Mutex<()> {
  STM_COMMIT_LOCK.get_or_init(|| Mutex::new(()))
}

// ── transaction log ────────────────────────────────────────────────────────

/// One attempt of a transactional read/write set.
pub struct Txn {
  /// `TRef` identity → shadow value for reads/writes in this attempt.
  shadow: HashMap<usize, Box<dyn Any + Send>>,
  /// Ids for which we recorded a version check against the live cell.
  validated: HashSet<usize>,
  read_validators: Vec<Box<dyn Fn() -> bool + Send>>,
  /// Sorted at commit: `id` → apply write + bump version.
  commit_writes: HashMap<usize, Box<dyn FnOnce() + Send>>,
}

impl Txn {
  fn new() -> Self {
    Self {
      shadow: HashMap::new(),
      validated: HashSet::new(),
      read_validators: Vec::new(),
      commit_writes: HashMap::new(),
    }
  }
}

fn try_commit(mut txn: Txn) -> Result<(), ()> {
  let _guard = commit_lock().lock().expect("stm: commit mutex poisoned");
  for v in &txn.read_validators {
    if !v() {
      return Err(());
    }
  }
  let mut ids: Vec<usize> = txn.commit_writes.keys().copied().collect();
  ids.sort_unstable();
  for id in ids {
    let w = txn
      .commit_writes
      .remove(&id)
      .expect("stm: commit_writes must contain sorted ids");
    w();
  }
  Ok(())
}

// ── STM outcome / program ───────────────────────────────────────────────────

/// Result of one [`Stm`] execution pass over a [`Txn`] (no I/O yet).
pub enum Outcome<A, E> {
  /// Transactional step finished with value `A` (commit may still fail validation).
  Done(A),
  /// Transactional failure; no commit attempted.
  Fail(E),
  /// Restart this attempt after yielding (blocking retry).
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

  /// Always retry (e.g. blocking queue take).
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

  /// On retry from `self`, run `that`. Failures propagate.
  pub fn or_else(self, that: Stm<A, E>) -> Stm<A, E> {
    let r1 = self.run.clone();
    let r2 = that.run.clone();
    Self::from_fn(move |txn| match r1(txn) {
      Outcome::Retry => r2(txn),
      o => o,
    })
  }

  /// Runs this program against an in-flight [`Txn`].
  pub fn run_on(&self, txn: &mut Txn) -> Outcome<A, E> {
    (self.run)(txn)
  }
}

// ── TRef ────────────────────────────────────────────────────────────────────

struct Inner<A> {
  ver: AtomicU64,
  data: Mutex<A>,
}

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

  fn ensure_validated(&self, txn: &mut Txn) {
    let id = self.id();
    if txn.validated.contains(&id) {
      return;
    }
    let g = self.inner.data.lock().expect("stm: TRef mutex poisoned");
    let ver = self.inner.ver.load(Ordering::Acquire);
    let _ = g.clone();
    drop(g);
    txn.validated.insert(id);
    let inner = self.inner.clone();
    txn.read_validators.push(Box::new(move || {
      let _g = inner.data.lock().expect("stm: TRef mutex poisoned");
      inner.ver.load(Ordering::Acquire) == ver
    }));
  }

  /// Allocate a new cell (each retry creates a fresh cell unless hoisted outside `commit`).
  pub fn make(initial: A) -> Stm<TRef<A>, ()>
  where
    A: Clone,
  {
    Stm::from_fn(move |_| {
      Outcome::Done(TRef {
        inner: Arc::new(Inner {
          ver: AtomicU64::new(0),
          data: Mutex::new(initial.clone()),
        }),
      })
    })
  }

  /// Read the current transactional or live value.
  pub fn read_stm<E: Send + 'static>(&self) -> Stm<A, E> {
    let r = self.clone();
    Stm::from_fn(move |txn| {
      let id = r.id();
      if let Some(any) = txn.shadow.get(&id) {
        let v = any
          .downcast_ref::<A>()
          .expect("stm: shadow type mismatch")
          .clone();
        return Outcome::Done(v);
      }
      let g = r.inner.data.lock().expect("stm: TRef mutex poisoned");
      let ver = r.inner.ver.load(Ordering::Acquire);
      let val = g.clone();
      drop(g);
      if txn.validated.insert(id) {
        let inner = r.inner.clone();
        txn.read_validators.push(Box::new(move || {
          let _g = inner.data.lock().expect("stm: TRef mutex poisoned");
          inner.ver.load(Ordering::Acquire) == ver
        }));
      }
      // Cache snapshot so later reads in this txn do not observe a newer live value.
      txn
        .shadow
        .entry(id)
        .or_insert_with(|| Box::new(val.clone()));
      Outcome::Done(val)
    })
  }

  /// Writes `value` at commit time after recording validation for this cell.
  pub fn write_stm<E: Send + 'static>(&self, value: A) -> Stm<(), E> {
    let r = self.clone();
    Stm::from_fn(move |txn| {
      r.ensure_validated(txn);
      let id = r.id();
      txn.shadow.insert(id, Box::new(value.clone()));
      let inner = r.inner.clone();
      let v = value.clone();
      txn.commit_writes.insert(
        id,
        Box::new(move || {
          let mut g = inner.data.lock().expect("stm: TRef mutex poisoned");
          *g = v;
          inner.ver.fetch_add(1, Ordering::Release);
        }),
      );
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
      let cur = match r.read_stm::<E>().run_on(txn) {
        Outcome::Done(v) => v,
        Outcome::Fail(e) => return Outcome::Fail(e),
        Outcome::Retry => return Outcome::Retry,
      };
      r.write_stm::<E>(f(cur)).run_on(txn)
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
      let cur = match r.read_stm::<E>().run_on(txn) {
        Outcome::Done(v) => v,
        Outcome::Fail(e) => return Outcome::Fail(e),
        Outcome::Retry => return Outcome::Retry,
      };
      let (out, next) = f(cur);
      match r.write_stm::<E>(next).run_on(txn) {
        Outcome::Done(()) => Outcome::Done(out),
        Outcome::Fail(e) => Outcome::Fail(e),
        Outcome::Retry => Outcome::Retry,
      }
    })
  }

  fn read_txn<E: Send + 'static>(&self, txn: &mut Txn) -> A {
    match self.read_stm::<E>().run_on(txn) {
      Outcome::Done(v) => v,
      Outcome::Fail(_) | Outcome::Retry => {
        unreachable!("stm: TRef::read_stm inside txn must not fail/retry without propagating")
      }
    }
  }

  fn set_txn<E: Send + 'static>(&self, txn: &mut Txn, value: A) {
    let _ = self.write_stm::<E>(value).run_on(txn);
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
      let mut st = q.inner.read_txn::<E>(txn);
      if let Some(cap) = st.cap
        && st.buf.len() >= cap
      {
        return Outcome::Done(false);
      }
      st.buf.push_back(value.clone());
      q.inner.set_txn::<E>(txn, st);
      Outcome::Done(true)
    })
  }

  /// Remove the head element; [`Outcome::Retry`] while empty.
  pub fn take<E>(&self) -> Stm<A, E>
  where
    E: Send + 'static,
  {
    let q = self.clone();
    Stm::from_fn(move |txn| {
      let mut st = q.inner.read_txn::<E>(txn);
      if let Some(x) = st.buf.pop_front() {
        q.inner.set_txn::<E>(txn, st);
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
      let map = m.inner.read_txn::<E>(txn);
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
      let mut map = m.inner.read_txn::<E>(txn);
      map.insert(key.clone(), value.clone());
      m.inner.set_txn::<E>(txn, map);
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
      let mut map = m.inner.read_txn::<E>(txn);
      map.remove(&key);
      m.inner.set_txn::<E>(txn, map);
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
  /// Permit counter initialized to `permits` ([`TSemaphore::acquire`] retries while zero).
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
      let n = s.inner.read_txn::<E>(txn);
      if n == 0 {
        return Outcome::Retry;
      }
      s.inner.set_txn::<E>(txn, n - 1);
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
      let n = s.inner.read_txn::<E>(txn);
      s.inner.set_txn::<E>(txn, n + 1);
      Outcome::Done(())
    })
  }
}

// ── commit ─────────────────────────────────────────────────────────────────

/// Run `stm` until it commits successfully (sync [`Effect::new`] body; safe for [`crate::runtime::run_blocking`]).
pub fn commit<A, E, R>(stm: Stm<A, E>) -> Effect<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: 'static,
{
  let stm = stm.clone();
  Effect::new(move |_r| {
    loop {
      let mut txn = Txn::new();
      match stm.run_on(&mut txn) {
        Outcome::Fail(e) => return Err(e),
        Outcome::Retry => {
          std::thread::yield_now();
          continue;
        }
        Outcome::Done(a) => match try_commit(txn) {
          Ok(()) => return Ok(a),
          Err(()) => {
            std::thread::yield_now();
            continue;
          }
        },
      }
    }
  })
}

/// Alias for [`commit`] (Effect.ts naming).
pub fn atomically<A, E, R>(stm: Stm<A, E>) -> Effect<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: 'static,
{
  commit(stm)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::runtime::run_blocking;
  use std::thread;
  use std::time::Duration;

  fn run<A: Send + 'static, E: Send + 'static>(stm: Stm<A, E>) -> Result<A, E> {
    run_blocking(commit(stm), ())
  }

  mod stm_constructors {
    use super::*;

    #[test]
    fn succeed_returns_value_immediately() {
      assert_eq!(run(Stm::<i32, ()>::succeed(42)), Ok(42));
    }

    #[test]
    fn fail_returns_error_immediately() {
      assert_eq!(run(Stm::<i32, &str>::fail("boom")), Err("boom"));
    }

    #[test]
    fn check_true_returns_unit_success() {
      assert_eq!(run(Stm::<(), ()>::check(true)), Ok(()));
    }
  }

  mod stm_functor {
    use super::*;

    #[test]
    fn map_transforms_success_value() {
      let stm = Stm::<i32, ()>::succeed(3).map(|n| n * 2);
      assert_eq!(run(stm), Ok(6));
    }

    #[test]
    fn map_preserves_failure() {
      let stm = Stm::<i32, &str>::fail("err").map(|n| n + 1);
      assert_eq!(run(stm), Err("err"));
    }
  }

  mod stm_monad {
    use super::*;

    #[test]
    fn flat_map_sequences_two_succeed_values() {
      let stm = Stm::<i32, ()>::succeed(5).flat_map(|n| Stm::succeed(n + 1));
      assert_eq!(run(stm), Ok(6));
    }

    #[test]
    fn flat_map_propagates_first_failure() {
      let stm = Stm::<i32, &str>::fail("e").flat_map(|_| Stm::succeed(0));
      assert_eq!(run(stm), Err("e"));
    }

    #[test]
    fn or_else_picks_second_when_first_retries() {
      let stm = Stm::<i32, ()>::retry().or_else(Stm::succeed(7));
      assert_eq!(run(stm), Ok(7));
    }

    #[test]
    fn or_else_preserves_first_success() {
      let stm = Stm::<i32, ()>::succeed(1).or_else(Stm::succeed(99));
      assert_eq!(run(stm), Ok(1));
    }

    #[test]
    fn or_else_propagates_failure_without_trying_second() {
      let stm = Stm::<i32, &str>::fail("e").or_else(Stm::succeed(0));
      assert_eq!(run(stm), Err("e"));
    }
  }

  mod tref {
    use super::*;

    #[test]
    fn make_creates_tref_with_initial_value() {
      let got = run(TRef::make(10i32).flat_map(|r| r.read_stm::<()>()));
      assert_eq!(got, Ok(10));
    }

    #[test]
    fn write_then_read_returns_new_value() {
      let got = run(TRef::make(0i32).flat_map(|r| {
        let r2 = r.clone();
        r.write_stm::<()>(42).flat_map(move |_| r2.read_stm::<()>())
      }));
      assert_eq!(got, Ok(42));
    }

    #[test]
    fn update_applies_function_to_value() {
      let got = run(TRef::make(3i32).flat_map(|r| {
        let r2 = r.clone();
        r.update_stm::<(), _>(|n| n * 2)
          .flat_map(move |_| r2.read_stm::<()>())
      }));
      assert_eq!(got, Ok(6));
    }

    #[test]
    fn modify_applies_and_returns_derived_value() {
      let got = run(TRef::make(5i32).flat_map(|r| r.modify_stm::<i32, (), _>(|n| (n + 1, n))));
      assert_eq!(got, Ok(6));
    }
    #[test]
    fn tref_update_visible_after_commit() {
      let prog = TRef::make(0i32).flat_map(|r| {
        let r2 = r.clone();
        r.write_stm::<()>(42).flat_map(move |_| r2.read_stm::<()>())
      });
      let got = run_blocking(commit(prog), ()).expect("commit");
      assert_eq!(got, 42);
    }
  }

  mod tref_concurrent {
    use super::*;

    #[test]
    fn stm_retry_waits_for_tref_change() {
      let cell = run_blocking(commit(TRef::make(0u32)), ()).expect("cell");
      let cell_w = cell.clone();
      let waiter = thread::spawn(move || {
        run_blocking(
          commit(Stm::<u32, ()>::from_fn(move |txn| {
            let n = cell_w.read_txn::<()>(txn);
            if n == 0 {
              Outcome::Retry
            } else {
              Outcome::Done(n)
            }
          })),
          (),
        )
        .expect("waiter")
      });
      thread::sleep(Duration::from_millis(20));
      run_blocking(commit(cell.write_stm::<()>(1)), ()).expect("set");
      assert_eq!(waiter.join().expect("join"), 1);
    }
  }

  mod tmap {
    use super::*;

    #[test]
    fn set_then_get_returns_inserted_value() {
      let got = run(TMap::<&str, i32>::make().flat_map(|m| {
        let m2 = m.clone();
        m.set::<()>("k", 42).flat_map(move |_| m2.get::<()>(&"k"))
      }));
      assert_eq!(got, Ok(Some(42)));
    }

    #[test]
    fn get_absent_key_returns_none() {
      let got = run(TMap::<&str, i32>::make().flat_map(|m| m.get::<()>(&"missing")));
      assert_eq!(got, Ok(None));
    }

    #[test]
    fn delete_removes_key() {
      let got = run(TMap::<&str, i32>::make().flat_map(|m| {
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
      let got = run(TQueue::<i32>::unbounded().flat_map(|q| {
        let q2 = q.clone();
        q.offer::<()>(99).flat_map(move |_| q2.take::<()>())
      }));
      assert_eq!(got, Ok(99));
    }

    #[test]
    fn tqueue_take_retries_until_offer() {
      let q: TQueue<i32> = run_blocking(commit(TQueue::unbounded()), ()).expect("make queue");
      let q_c = q.clone();
      let take = thread::spawn(move || {
        let v = run_blocking(commit(q_c.take::<()>()), ()).expect("take");
        assert_eq!(v, 7);
      });
      thread::sleep(Duration::from_millis(20));
      run_blocking(commit(q.clone().offer::<()>(7)), ()).expect("offer");
      take.join().expect("join");
    }

    #[test]
    fn bounded_queue_offer_returns_true_when_capacity_available() {
      let got = run(TQueue::<i32>::bounded(2).flat_map(|q| q.offer::<()>(1)));
      assert_eq!(got, Ok(true));
    }
  }

  mod tsemaphore {
    use super::*;

    #[test]
    fn acquire_then_release_increments_permits_back() {
      let got = run(TSemaphore::make(1).flat_map(|s| {
        let s2 = s.clone();
        s.acquire::<()>().flat_map(move |_| s2.release::<()>())
      }));
      assert_eq!(got, Ok(()));
    }

    #[test]
    fn tsemaphore_acquire_blocks_when_zero() {
      let sem = run_blocking(commit(TSemaphore::make(0usize)), ()).expect("make sem");
      let sem_c = sem.clone();
      let acq = thread::spawn(move || {
        run_blocking(commit(sem_c.acquire::<()>()), ()).expect("acquire");
      });
      thread::sleep(Duration::from_millis(20));
      run_blocking(commit(sem.clone().release::<()>()), ()).expect("release");
      acq.join().expect("join");
    }
  }

  mod atomically {
    use super::*;

    #[test]
    fn atomically_is_alias_for_commit() {
      let got: Result<i32, ()> = run_blocking(atomically(Stm::succeed(5)), ());
      assert_eq!(got, Ok(5));
    }
  }
}
