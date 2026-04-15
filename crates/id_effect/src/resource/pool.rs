//! Managed resource pools — capacity gate, idle reuse, optional TTL, invalidation.
//!
//! Built from [`crate::coordination::semaphore::Semaphore`], [`crate::coordination::synchronized_ref::SynchronizedRef`], and
//! [`crate::resource::scope::Scope`] finalizers (check-in on scope close). Waiting for capacity is the
//! semaphore acquire; an idle slot becomes available when a prior checkout’s scope ends.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::coordination::semaphore::Semaphore;
use crate::coordination::synchronized_ref::SynchronizedRef;
use crate::kernel::{Effect, box_future};
use crate::resource::scope::Scope;
use crate::runtime::{Never, run_blocking};

// ── Pool state ──────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
struct IdleSlot<A> {
  value: A,
  at: Instant,
}

#[derive(Clone, Debug)]
struct PoolState<A> {
  idle: Vec<IdleSlot<A>>,
  discard: Vec<A>,
}

impl<A> Default for PoolState<A> {
  fn default() -> Self {
    Self {
      idle: Vec::new(),
      discard: Vec::new(),
    }
  }
}

fn take_idle<A: Clone + PartialEq>(pool: &mut PoolState<A>, ttl: Option<Duration>) -> Option<A> {
  let now = Instant::now();
  while !pool.idle.is_empty() {
    let slot = &pool.idle[0];
    if ttl.is_some_and(|t| now.duration_since(slot.at) > t) {
      pool.idle.remove(0);
      continue;
    }
    if pool.discard.iter().any(|d| d == &slot.value) {
      pool.idle.remove(0);
      continue;
    }
    return Some(pool.idle.remove(0).value);
  }
  None
}

// ── Pool ────────────────────────────────────────────────────────────────────

/// Fixed-capacity pool with reuse of returned values and optional TTL on idle slots.
#[derive(Clone)]
pub struct Pool<A: 'static, E: 'static> {
  sem: Semaphore,
  state: SynchronizedRef<PoolState<A>>,
  factory: Arc<dyn Fn() -> Effect<A, E, ()> + Send + Sync>,
  ttl: Option<Duration>,
}

impl<A, E> Pool<A, E>
where
  A: Clone + PartialEq + Send + Sync + 'static,
  E: Send + Sync + 'static,
{
  /// Build a pool with `capacity` concurrent checkouts and `factory` for new values.
  pub fn make<F>(capacity: usize, factory: F) -> Effect<Self, Never, ()>
  where
    F: Fn() -> Effect<A, E, ()> + Send + Sync + 'static,
  {
    Effect::new(move |_r| {
      let sem = run_blocking(Semaphore::make(capacity), ()).expect("pool semaphore");
      let state =
        run_blocking(SynchronizedRef::make(PoolState::default()), ()).expect("pool state");
      Ok(Pool {
        sem,
        state,
        factory: Arc::new(factory),
        ttl: None,
      })
    })
  }

  /// Same as [`Self::make`], but idle slots older than `ttl` are discarded on checkout.
  pub fn make_with_ttl<F>(capacity: usize, ttl: Duration, factory: F) -> Effect<Self, Never, ()>
  where
    F: Fn() -> Effect<A, E, ()> + Send + Sync + 'static,
  {
    Pool::make(capacity, factory).map(move |mut p| {
      p.ttl = Some(ttl);
      p
    })
  }

  /// Mark `item` as not reusable; removes matching entries from the idle list. If the item is
  /// still checked out, it will not be returned to the idle list when its scope closes.
  pub fn invalidate(&self, item: A) -> Effect<(), Never, ()> {
    let state = self.state.clone();
    Effect::new_async(move |_r: &mut ()| {
      box_future(async move {
        let _ = run_blocking(
          state.update(move |mut p| {
            p.discard.push(item.clone());
            p.idle.retain(|s| s.value != item);
            p
          }),
          (),
        );
        Ok(())
      })
    })
  }

  /// Checkout a value: acquires capacity, then reuses an idle slot (respecting TTL) or runs
  /// `factory`. When the caller’s [`Scope`] closes, the value is returned to the idle list unless
  /// it was [`Self::invalidate`].
  pub fn get(&self) -> Effect<A, E, Scope> {
    let sem = self.sem.clone();
    let state = self.state.clone();
    let factory = Arc::clone(&self.factory);
    let ttl = self.ttl;
    Effect::new_async(move |scope: &mut Scope| {
      let scope = scope.clone();
      box_future(async move {
        let mut acquire_env = scope.clone();
        let _p = sem
          .acquire()
          .run(&mut acquire_env)
          .await
          .expect("semaphore acquire");

        let item = {
          let taken = run_blocking(
            state.modify(move |mut st| {
              let v = take_idle(&mut st, ttl);
              (v, st)
            }),
            (),
          )
          .expect("pool modify");
          match taken {
            Some(a) => a,
            None => factory().run(&mut ()).await?,
          }
        };

        let state_fin = state.clone();
        let item_fin = item.clone();
        let fin_scope = scope.clone();
        let _ = fin_scope.add_finalizer(Box::new(move |_exit| {
          let st = state_fin.clone();
          let val = item_fin.clone();
          Effect::new_async(move |_r: &mut ()| {
            box_future(async move {
              run_blocking(
                st.update(move |mut p| {
                  if !p.discard.iter().any(|d| d == &val) {
                    p.idle.push(IdleSlot {
                      value: val,
                      at: Instant::now(),
                    });
                  }
                  p
                }),
                (),
              )
              .expect("pool check-in");
              Ok::<(), Never>(())
            })
          })
        }));

        Ok(item)
      })
    })
  }
}

// ── Keyed pool ──────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
struct KeyedPoolState<K, A> {
  per_key: HashMap<K, Vec<IdleSlot<A>>>,
  discard: Vec<(K, A)>,
}

impl<K, A> Default for KeyedPoolState<K, A> {
  fn default() -> Self {
    Self {
      per_key: HashMap::new(),
      discard: Vec::new(),
    }
  }
}

fn take_idle_keyed<K: Clone + Eq + std::hash::Hash, A: Clone + PartialEq>(
  st: &mut KeyedPoolState<K, A>,
  key: &K,
  ttl: Option<Duration>,
) -> Option<A> {
  let now = Instant::now();
  let slots = st.per_key.get_mut(key)?;
  while !slots.is_empty() {
    let slot = &slots[0];
    if ttl.is_some_and(|t| now.duration_since(slot.at) > t) {
      slots.remove(0);
      continue;
    }
    if st.discard.iter().any(|(k, a)| k == key && a == &slot.value) {
      slots.remove(0);
      continue;
    }
    let v = slots.remove(0).value;
    if slots.is_empty() {
      st.per_key.remove(key);
    }
    return Some(v);
  }
  st.per_key.remove(key);
  None
}

/// Pool partitioned by key; total concurrent checkouts across all keys is bounded by `capacity`.
#[derive(Clone)]
pub struct KeyedPool<K: 'static, A: 'static, E: 'static> {
  sem: Semaphore,
  state: SynchronizedRef<KeyedPoolState<K, A>>,
  factory: Arc<dyn Fn(K) -> Effect<A, E, ()> + Send + Sync>,
  ttl: Option<Duration>,
}

impl<K, A, E> KeyedPool<K, A, E>
where
  K: Clone + Eq + std::hash::Hash + Send + Sync + 'static,
  A: Clone + PartialEq + Send + Sync + 'static,
  E: Send + Sync + 'static,
{
  /// Build a keyed pool with global `capacity` and per-key `factory` for new values.
  pub fn make<F>(capacity: usize, factory: F) -> Effect<Self, Never, ()>
  where
    F: Fn(K) -> Effect<A, E, ()> + Send + Sync + 'static,
  {
    Effect::new(move |_r| {
      let sem = run_blocking(Semaphore::make(capacity), ()).expect("keyed pool semaphore");
      let state =
        run_blocking(SynchronizedRef::make(KeyedPoolState::default()), ()).expect("keyed state");
      Ok(KeyedPool {
        sem,
        state,
        factory: Arc::new(factory),
        ttl: None,
      })
    })
  }

  /// Same as [`Self::make`], but idle slots older than `ttl` are discarded on checkout.
  pub fn make_with_ttl<F>(capacity: usize, ttl: Duration, factory: F) -> Effect<Self, Never, ()>
  where
    F: Fn(K) -> Effect<A, E, ()> + Send + Sync + 'static,
  {
    KeyedPool::make(capacity, factory).map(move |mut p| {
      p.ttl = Some(ttl);
      p
    })
  }

  /// Mark `(key, item)` as not reusable for that key; drops matching idle entries.
  pub fn invalidate(&self, key: K, item: A) -> Effect<(), Never, ()> {
    let state = self.state.clone();
    Effect::new_async(move |_r: &mut ()| {
      box_future(async move {
        let _ = run_blocking(
          state.update(move |mut st| {
            st.discard.push((key.clone(), item.clone()));
            if let Some(slots) = st.per_key.get_mut(&key) {
              slots.retain(|s| s.value != item);
              if slots.is_empty() {
                st.per_key.remove(&key);
              }
            }
            st
          }),
          (),
        );
        Ok(())
      })
    })
  }

  /// Checkout a value for `key`: acquires global capacity, then reuses idle or runs `factory`.
  pub fn get(&self, key: K) -> Effect<A, E, Scope> {
    let sem = self.sem.clone();
    let state = self.state.clone();
    let factory = Arc::clone(&self.factory);
    let ttl = self.ttl;
    Effect::new_async(move |scope: &mut Scope| {
      let scope = scope.clone();
      let key_for_fin = key.clone();
      box_future(async move {
        let mut acquire_env = scope.clone();
        let _p = sem
          .acquire()
          .run(&mut acquire_env)
          .await
          .expect("semaphore acquire");

        let item = {
          let key_borrow = key.clone();
          let taken = run_blocking(
            state.modify(move |mut st| {
              let v = take_idle_keyed(&mut st, &key_borrow, ttl);
              (v, st)
            }),
            (),
          )
          .expect("keyed pool modify");
          match taken {
            Some(a) => a,
            None => factory(key).run(&mut ()).await?,
          }
        };

        let state_fin = state.clone();
        let item_fin = item.clone();
        let kfin = key_for_fin.clone();
        let fin_scope = scope.clone();
        let _ = fin_scope.add_finalizer(Box::new(move |_exit| {
          let st = state_fin.clone();
          let val = item_fin.clone();
          let k = kfin.clone();
          Effect::new_async(move |_r: &mut ()| {
            box_future(async move {
              run_blocking(
                st.update(move |mut p| {
                  if !p.discard.iter().any(|(dk, da)| dk == &k && da == &val) {
                    p.per_key.entry(k.clone()).or_default().push(IdleSlot {
                      value: val,
                      at: Instant::now(),
                    });
                  }
                  p
                }),
                (),
              )
              .expect("keyed pool check-in");
              Ok::<(), Never>(())
            })
          })
        }));

        Ok(item)
      })
    })
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::kernel::succeed;
  use crate::runtime::run_async;
  use std::sync::atomic::{AtomicUsize, Ordering};
  use std::sync::mpsc;
  use std::thread;

  #[tokio::test]
  async fn pool_get_blocks_when_exhausted() {
    let factory_calls = Arc::new(AtomicUsize::new(0));
    let fc = factory_calls.clone();
    let pool = run_blocking(
      Pool::make(1, move || {
        fc.fetch_add(1, Ordering::SeqCst);
        succeed::<u32, (), ()>(7)
      }),
      (),
    )
    .expect("make pool");

    let (tx, rx) = mpsc::channel::<()>();
    let pool_t = pool.clone();
    let th = thread::spawn(move || {
      let scope = Scope::make();
      pollster::block_on(run_async(pool_t.get(), scope.clone())).expect("get");
      tx.send(()).expect("signal");
      thread::sleep(Duration::from_millis(120));
      scope.close();
    });
    rx.recv().expect("peer hold");

    let scope_m = Scope::make();
    let start = Instant::now();
    run_async(pool.get(), scope_m.clone())
      .await
      .expect("second get");
    assert!(
      start.elapsed() >= Duration::from_millis(50),
      "expected second get to block until first scope closed"
    );
    scope_m.close();
    th.join().expect("thread");
    assert_eq!(factory_calls.load(Ordering::SeqCst), 1);
  }

  #[tokio::test]
  async fn pool_item_released_on_scope_close() {
    let fc = Arc::new(AtomicUsize::new(0));
    let fc2 = fc.clone();
    let pool = run_blocking(
      Pool::make(2, move || {
        fc2.fetch_add(1, Ordering::SeqCst);
        succeed::<u32, (), ()>(42)
      }),
      (),
    )
    .expect("pool");

    let s1 = Scope::make();
    let _ = run_async(pool.get(), s1.clone()).await.expect("g1");
    s1.close();
    let s2 = Scope::make();
    let _ = run_async(pool.get(), s2.clone()).await.expect("g2");
    s2.close();

    assert_eq!(
      fc.load(Ordering::SeqCst),
      1,
      "factory should run once; second get reuses idle"
    );
  }

  #[tokio::test]
  async fn pool_invalidate_forces_remake_on_next_get() {
    let fc = Arc::new(AtomicUsize::new(0));
    let fc2 = fc.clone();
    let pool = run_blocking(
      Pool::make(2, move || {
        fc2.fetch_add(1, Ordering::SeqCst);
        succeed::<u32, (), ()>(100 + fc2.load(Ordering::SeqCst) as u32)
      }),
      (),
    )
    .expect("pool");

    let s = Scope::make();
    let v = run_async(pool.get(), s.clone()).await.expect("get");
    run_blocking(pool.invalidate(v), ()).expect("invalidate");
    s.close();

    let s2 = Scope::make();
    let v2 = run_async(pool.get(), s2.clone()).await.expect("get2");
    assert_ne!(v, v2);
    assert_eq!(fc.load(Ordering::SeqCst), 2);
    s2.close();
  }

  #[tokio::test]
  async fn keyed_pool_isolates_keys_by_factory_calls() {
    let fc = Arc::new(AtomicUsize::new(0));
    let fc2 = fc.clone();
    let pool = run_blocking(
      KeyedPool::make(4, move |k: &'static str| {
        fc2.fetch_add(1, Ordering::SeqCst);
        succeed::<String, (), ()>(format!("{k}-{}", fc2.load(Ordering::SeqCst)))
      }),
      (),
    )
    .expect("keyed");

    let sa = Scope::make();
    let sb = Scope::make();
    let _ = run_async(pool.get("a"), sa.clone()).await.unwrap();
    let _ = run_async(pool.get("b"), sb.clone()).await.unwrap();
    sa.close();
    sb.close();

    assert_eq!(fc.load(Ordering::SeqCst), 2);

    let sa2 = Scope::make();
    let v = run_async(pool.get("a"), sa2.clone()).await.unwrap();
    assert!(v.starts_with("a-"));
    sa2.close();
    assert_eq!(fc.load(Ordering::SeqCst), 2, "reuse for key a");
  }

  // ── Pool::make_with_ttl ───────────────────────────────────────────────────

  #[tokio::test]
  async fn pool_make_with_ttl_evicts_stale_idle_slot() {
    let fc = Arc::new(AtomicUsize::new(0));
    let fc2 = fc.clone();
    let pool = run_blocking(
      Pool::make_with_ttl(2, Duration::from_millis(20), move || {
        fc2.fetch_add(1, Ordering::SeqCst);
        succeed::<u32, (), ()>(fc2.load(Ordering::SeqCst) as u32)
      }),
      (),
    )
    .expect("pool with ttl");

    let s1 = Scope::make();
    let _ = run_async(pool.get(), s1.clone()).await.expect("get1");
    s1.close();

    // Wait for TTL to expire
    tokio::time::sleep(Duration::from_millis(50)).await;

    let s2 = Scope::make();
    let _ = run_async(pool.get(), s2.clone()).await.expect("get2");
    s2.close();

    assert_eq!(
      fc.load(Ordering::SeqCst),
      2,
      "stale idle slot should be discarded, factory called again"
    );
  }

  // ── KeyedPool::make_with_ttl ──────────────────────────────────────────────

  #[tokio::test]
  async fn keyed_pool_make_with_ttl_evicts_stale_idle_slot() {
    let fc = Arc::new(AtomicUsize::new(0));
    let fc2 = fc.clone();
    let pool = run_blocking(
      KeyedPool::make_with_ttl(4, Duration::from_millis(20), move |_k: &'static str| {
        fc2.fetch_add(1, Ordering::SeqCst);
        succeed::<u32, (), ()>(fc2.load(Ordering::SeqCst) as u32)
      }),
      (),
    )
    .expect("keyed pool with ttl");

    let s1 = Scope::make();
    let _ = run_async(pool.get("key"), s1.clone()).await.expect("get1");
    s1.close();

    tokio::time::sleep(Duration::from_millis(50)).await;

    let s2 = Scope::make();
    let _ = run_async(pool.get("key"), s2.clone()).await.expect("get2");
    s2.close();

    assert_eq!(
      fc.load(Ordering::SeqCst),
      2,
      "stale idle slot for keyed pool should be discarded"
    );
  }

  // ── KeyedPool::invalidate ─────────────────────────────────────────────────

  #[tokio::test]
  async fn keyed_pool_invalidate_forces_factory_on_next_get() {
    let fc = Arc::new(AtomicUsize::new(0));
    let fc2 = fc.clone();
    let pool = run_blocking(
      KeyedPool::make(4, move |k: &'static str| {
        fc2.fetch_add(1, Ordering::SeqCst);
        succeed::<String, (), ()>(format!("{}-{}", k, fc2.load(Ordering::SeqCst)))
      }),
      (),
    )
    .expect("keyed pool");

    let s1 = Scope::make();
    let v = run_async(pool.get("x"), s1.clone()).await.expect("get1");
    run_blocking(pool.invalidate("x", v.clone()), ()).expect("invalidate");
    s1.close();

    let s2 = Scope::make();
    let v2 = run_async(pool.get("x"), s2.clone()).await.expect("get2");
    s2.close();

    assert_ne!(v, v2, "invalidated item should not be reused");
    assert_eq!(fc.load(Ordering::SeqCst), 2, "factory should run twice");
  }
}
