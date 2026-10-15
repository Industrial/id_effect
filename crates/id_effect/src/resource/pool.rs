//! `Pool` and `KeyedPool`: bounded, reusable resources checked out into a [`Scope`] (Effect.ts
//! `Pool` / `KeyedPool`).
//!
//! A checkout ([`Pool::get`]) waits for capacity, then reuses an idle item or acquires a new
//! one, and registers a finalizer on the caller's scope that checks the item back in. The
//! check-in is a scope finalizer, so it runs however the scope closes: success, failure, defect
//! or **interrupt**. Waiting for capacity is interruptible and never leaks capacity; acquiring
//! a new item runs uninterruptibly (Effect.ts `acquireRelease`).

use std::collections::{HashMap, VecDeque};
use std::hash::Hash;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::gate::{Gate, Permit, interrupt_self, lock, under_any_env, with_now};
use crate::failure::Never;
use crate::next::capability::HNil;
use crate::next::kernel::{Effect, succeed, suspend, uninterruptible_mask};
use crate::next::scope::Scope;

type AcquireFn<A, E, R> = Arc<dyn Fn() -> Effect<A, E, R> + Send + Sync>;
type ReleaseFn<A> = Arc<dyn Fn(A) -> Effect<(), Never, HNil> + Send + Sync>;

/// Runs `release` on each item in order (nothing when there is no release hook). Each hook is
/// called inside a `Sync` step, so a panic becomes `Die`.
fn release_all<A: Send + 'static>(
  release: &Option<ReleaseFn<A>>,
  items: Vec<A>,
) -> Effect<(), Never, HNil> {
  match release {
    None => succeed(()),
    Some(rel) => items.into_iter().fold(succeed(()), |acc, item| {
      let rel = Arc::clone(rel);
      acc.then(suspend(move || rel(item)))
    }),
  }
}

// ── Pool ────────────────────────────────────────────────────────────────────────────────────

struct Idle<A> {
  value: A,
  since: Instant,
}

struct PoolState<A> {
  /// Checked-in items, oldest first.
  idle: VecDeque<Idle<A>>,
  /// Checked-out items.
  out: Vec<A>,
  /// Checked-out items invalidated while out: released at check-in instead of reused.
  invalid: Vec<A>,
  shut: bool,
}

struct Shared<A> {
  gate: Arc<Gate>,
  state: Mutex<PoolState<A>>,
}

/// A pool of at most `capacity` concurrently checked-out items, reusing checked-in ones
/// (Effect.ts `Pool`).
///
/// Built with a plain constructor; clones share the pool. Items are compared with `PartialEq`
/// for [`invalidate`](Self::invalidate), and handed out by clone, so a pooled connection is
/// typically an `Arc` or a cheap handle.
///
/// ```
/// use id_effect::Exit;
/// use id_effect::next::{Effect, Pool, succeed};
/// use id_effect::Never;
///
/// let pool: Pool<u32> = Pool::new(2, || succeed(7));
/// let p = pool.clone();
/// let eff: Effect<u32, Never> = Effect::scoped(move |scope| p.get(&scope));
/// assert!(matches!(eff.block_on(), Exit::Success(7)));
/// // The scope closed, so the item was checked back in.
/// assert_eq!((pool.idle_count(), pool.in_use()), (1, 0));
/// ```
pub struct Pool<A, E = Never, R = HNil> {
  shared: Arc<Shared<A>>,
  acquire: AcquireFn<A, E, R>,
  release: Option<ReleaseFn<A>>,
  ttl: Option<Duration>,
}

impl<A, E, R> Clone for Pool<A, E, R> {
  fn clone(&self) -> Self {
    Self {
      shared: Arc::clone(&self.shared),
      acquire: Arc::clone(&self.acquire),
      release: self.release.clone(),
      ttl: self.ttl,
    }
  }
}

impl<A, E, R> std::fmt::Debug for Pool<A, E, R> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    let st = lock(&self.shared.state);
    f.debug_struct("Pool")
      .field("capacity", &self.shared.gate.capacity())
      .field("in_use", &self.shared.gate.in_use())
      .field("idle", &st.idle.len())
      .field("ttl", &self.ttl)
      .field("shut", &st.shut)
      .finish()
  }
}

impl<A, E, R> Pool<A, E, R>
where
  A: Clone + PartialEq + Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  /// A pool of at most `capacity` checked-out items; `acquire` builds a new item when no idle
  /// one is available. A plain constructor: nothing is acquired until the first checkout.
  pub fn new(
    capacity: usize,
    acquire: impl Fn() -> Effect<A, E, R> + Send + Sync + 'static,
  ) -> Self {
    Self::from_parts(capacity, Arc::new(acquire), None, None)
  }

  fn from_parts(
    capacity: usize,
    acquire: AcquireFn<A, E, R>,
    release: Option<ReleaseFn<A>>,
    ttl: Option<Duration>,
  ) -> Self {
    Self {
      shared: Arc::new(Shared {
        gate: Gate::new(capacity),
        state: Mutex::new(PoolState {
          idle: VecDeque::new(),
          out: Vec::new(),
          invalid: Vec::new(),
          shut: false,
        }),
      }),
      acquire,
      release,
      ttl,
    }
  }

  /// Sets the hook that disposes of an item for good: when it is invalidated, expires, or the
  /// pool shuts down. It runs uninterruptibly. Configure before cloning the pool.
  #[must_use]
  pub fn with_release(
    mut self,
    release: impl Fn(A) -> Effect<(), Never, HNil> + Send + Sync + 'static,
  ) -> Self {
    self.release = Some(Arc::new(release));
    self
  }

  /// Idle items checked in more than `ttl` ago (on the executor clock) are released instead of
  /// reused at the next checkout. Configure before cloning the pool.
  #[must_use]
  pub fn with_ttl(mut self, ttl: Duration) -> Self {
    self.ttl = Some(ttl);
    self
  }

  /// The maximum number of concurrently checked-out items.
  pub fn capacity(&self) -> usize {
    self.shared.gate.capacity()
  }

  /// Items currently checked out (or being acquired).
  pub fn in_use(&self) -> usize {
    self.shared.gate.in_use()
  }

  /// Checked-in items ready for reuse.
  pub fn idle_count(&self) -> usize {
    lock(&self.shared.state).idle.len()
  }

  /// Fibers currently waiting for capacity.
  pub fn waiting(&self) -> usize {
    self.shared.gate.waiting()
  }

  /// `true` once [`shutdown`](Self::shutdown) has run.
  pub fn is_shutdown(&self) -> bool {
    lock(&self.shared.state).shut
  }

  /// Checks an item out into `scope` (Effect.ts `Pool.get`).
  ///
  /// Waits (interruptibly) until fewer than `capacity` items are out, then reuses the oldest
  /// fresh idle item or runs `acquire` (uninterruptibly). When `scope` closes, however it
  /// closes, interrupt included, the item is checked back in, or released if it was
  /// [invalidated](Self::invalidate) or the pool has shut down. A failing `acquire` gives the
  /// capacity back. On a shut-down pool the checkout is interrupted.
  pub fn get(&self, scope: &Scope) -> Effect<A, E, R> {
    let pool = self.clone();
    let scope = scope.clone();
    uninterruptible_mask(move |restore| {
      restore
        .apply(pool.shared.gate.acquire())
        .flat_map(move |permit| match permit {
          None => interrupt_self(),
          Some(permit) => with_now(move |now| pool.checkout(permit, scope, now)),
        })
    })
  }

  /// The masked part of a checkout, holding `permit`.
  fn checkout(self, permit: Permit, scope: Scope, now: Instant) -> Effect<A, E, R> {
    let (item, stale, shut) = {
      let mut st = lock(&self.shared.state);
      let mut stale = Vec::new();
      let mut item = None;
      while let Some(slot) = st.idle.pop_front() {
        if self
          .ttl
          .is_some_and(|ttl| now.saturating_duration_since(slot.since) >= ttl)
        {
          stale.push(slot.value);
        } else {
          item = Some(slot.value);
          break;
        }
      }
      (item, stale, st.shut)
    };
    if shut {
      drop(permit);
      return under_any_env(release_all(
        &self.release,
        item.into_iter().chain(stale).collect(),
      ))
      .widen()
      .flat_map(|()| interrupt_self());
    }
    let acquire = Arc::clone(&self.acquire);
    let item_eff: Effect<A, E, R> = match item {
      Some(a) => succeed(a),
      None => suspend(move || acquire()),
    };
    under_any_env(release_all(&self.release, stale))
      .widen()
      .flat_map(move |()| item_eff)
      .flat_map(move |item| {
        lock(&self.shared.state).out.push(item.clone());
        let fin = self.check_in(item.clone(), permit);
        scope.add_finalizer(fin).map(move |()| item)
      })
  }

  /// The check-in finalizer: back to the idle list, or released when invalidated / shut down.
  /// The permit is given back last, so a waiter it wakes finds the item idle.
  fn check_in(&self, item: A, permit: Permit) -> Effect<(), Never, HNil> {
    let shared = Arc::clone(&self.shared);
    let release = self.release.clone();
    with_now(move |now| {
      let dispose = {
        let mut st = lock(&shared.state);
        if let Some(pos) = st.out.iter().position(|a| *a == item) {
          st.out.swap_remove(pos);
        }
        let invalid = match st.invalid.iter().position(|a| *a == item) {
          Some(pos) => {
            st.invalid.swap_remove(pos);
            true
          }
          None => false,
        };
        if invalid || st.shut {
          Some(item)
        } else {
          st.idle.push_back(Idle {
            value: item,
            since: now,
          });
          None
        }
      };
      release_all(&release, dispose.into_iter().collect()).map(move |()| drop(permit))
    })
  }

  /// Marks `item` as unusable (Effect.ts `Pool.invalidate`): idle copies are released now, and
  /// a checked-out copy is released at check-in instead of being reused.
  pub fn invalidate<E2: Send + 'static, R2: Send + Sync + 'static>(
    &self,
    item: &A,
  ) -> Effect<(), E2, R2> {
    let shared = Arc::clone(&self.shared);
    let release = self.release.clone();
    let item = item.clone();
    under_any_env(
      suspend(move || {
        let dropped = {
          let mut st = lock(&shared.state);
          let mut dropped = Vec::new();
          st.idle.retain(|slot| {
            if slot.value == item {
              dropped.push(slot.value.clone());
              false
            } else {
              true
            }
          });
          if st.out.contains(&item) && !st.invalid.contains(&item) {
            st.invalid.push(item);
          }
          dropped
        };
        release_all(&release, dropped)
      })
      .widen(),
    )
  }

  /// Shuts the pool down: idle items are released, waiters and later checkouts are
  /// interrupted, and items still out are released when they are checked in. Idempotent.
  ///
  /// The effect is `HNil` and infallible, so it can be registered as a scope finalizer
  /// (`scope.add_finalizer(pool.shutdown())`) to tie the pool's lifetime to a scope.
  pub fn shutdown(&self) -> Effect<(), Never, HNil> {
    let shared = Arc::clone(&self.shared);
    let release = self.release.clone();
    suspend(move || {
      let idle = {
        let mut st = lock(&shared.state);
        st.shut = true;
        st.idle.drain(..).map(|slot| slot.value).collect()
      };
      shared.gate.close();
      release_all(&release, idle)
    })
    .uninterruptible()
  }
}

// ── KeyedPool ───────────────────────────────────────────────────────────────────────────────

type KeyedAcquireFn<K, A, E, R> = Arc<dyn Fn(K) -> Effect<A, E, R> + Send + Sync>;
type CapacityFn<K> = Arc<dyn Fn(&K) -> usize + Send + Sync>;

struct KeyedState<K, A, E, R> {
  pools: HashMap<K, Pool<A, E, R>>,
  shut: bool,
}

/// A pool partitioned by key: each key has its own [`Pool`] of `capacity` items, created on
/// first use (Effect.ts `KeyedPool`). Checking out under one key never waits on another.
///
/// ```
/// use id_effect::Exit;
/// use id_effect::next::{Effect, KeyedPool, succeed};
/// use id_effect::Never;
///
/// let pool: KeyedPool<&str, String> = KeyedPool::new(1, |k: &str| succeed(format!("conn-{k}")));
/// let p = pool.clone();
/// let eff: Effect<(String, String), Never> =
///   Effect::scoped(move |scope| p.get("a", &scope).zip(p.get("b", &scope)));
/// assert!(matches!(eff.block_on(), Exit::Success((a, b)) if a == "conn-a" && b == "conn-b"));
/// ```
pub struct KeyedPool<K, A, E = Never, R = HNil> {
  state: Arc<Mutex<KeyedState<K, A, E, R>>>,
  capacity: CapacityFn<K>,
  acquire: KeyedAcquireFn<K, A, E, R>,
  release: Option<ReleaseFn<A>>,
  ttl: Option<Duration>,
}

impl<K, A, E, R> Clone for KeyedPool<K, A, E, R> {
  fn clone(&self) -> Self {
    Self {
      state: Arc::clone(&self.state),
      capacity: Arc::clone(&self.capacity),
      acquire: Arc::clone(&self.acquire),
      release: self.release.clone(),
      ttl: self.ttl,
    }
  }
}

impl<K, A, E, R> std::fmt::Debug for KeyedPool<K, A, E, R> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    let st = lock(&self.state);
    f.debug_struct("KeyedPool")
      .field("keys", &st.pools.len())
      .field("ttl", &self.ttl)
      .field("shut", &st.shut)
      .finish()
  }
}

impl<K, A, E, R> KeyedPool<K, A, E, R>
where
  K: Clone + Eq + Hash + Send + Sync + 'static,
  A: Clone + PartialEq + Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  /// A keyed pool with `capacity` items per key; `acquire(key)` builds a new item for a key.
  /// A plain constructor: nothing is acquired until the first checkout.
  pub fn new(
    capacity: usize,
    acquire: impl Fn(K) -> Effect<A, E, R> + Send + Sync + 'static,
  ) -> Self {
    Self::new_with(move |_| capacity, acquire)
  }

  /// A keyed pool whose per-key capacity is `capacity(&key)` (Effect.ts `KeyedPool.makeWith`).
  pub fn new_with(
    capacity: impl Fn(&K) -> usize + Send + Sync + 'static,
    acquire: impl Fn(K) -> Effect<A, E, R> + Send + Sync + 'static,
  ) -> Self {
    Self {
      state: Arc::new(Mutex::new(KeyedState {
        pools: HashMap::new(),
        shut: false,
      })),
      capacity: Arc::new(capacity),
      acquire: Arc::new(acquire),
      release: None,
      ttl: None,
    }
  }

  /// See [`Pool::with_release`]; applies to every key. Configure before the first checkout.
  #[must_use]
  pub fn with_release(
    mut self,
    release: impl Fn(A) -> Effect<(), Never, HNil> + Send + Sync + 'static,
  ) -> Self {
    self.release = Some(Arc::new(release));
    self
  }

  /// See [`Pool::with_ttl`]; applies to every key. Configure before the first checkout.
  #[must_use]
  pub fn with_ttl(mut self, ttl: Duration) -> Self {
    self.ttl = Some(ttl);
    self
  }

  /// The pool for `key`, created on first use (`None` once shut down).
  fn pool_for(&self, key: &K) -> Option<Pool<A, E, R>> {
    let mut st = lock(&self.state);
    if st.shut {
      return None;
    }
    let pool = st.pools.entry(key.clone()).or_insert_with(|| {
      let acquire = Arc::clone(&self.acquire);
      let k = key.clone();
      Pool::from_parts(
        (self.capacity)(key),
        Arc::new(move || acquire(k.clone())),
        self.release.clone(),
        self.ttl,
      )
    });
    Some(pool.clone())
  }

  /// Checks an item for `key` out into `scope`; see [`Pool::get`].
  pub fn get(&self, key: K, scope: &Scope) -> Effect<A, E, R> {
    let this = self.clone();
    let scope = scope.clone();
    suspend(move || match this.pool_for(&key) {
      Some(pool) => pool.get(&scope),
      None => interrupt_self(),
    })
  }

  /// Marks `item` of `key` as unusable; see [`Pool::invalidate`].
  pub fn invalidate<E2: Send + 'static, R2: Send + Sync + 'static>(
    &self,
    key: &K,
    item: &A,
  ) -> Effect<(), E2, R2> {
    let pool = lock(&self.state).pools.get(key).cloned();
    match pool {
      Some(pool) => pool.invalidate(item),
      None => succeed(()),
    }
  }

  /// Items of `key` currently checked out.
  pub fn in_use(&self, key: &K) -> usize {
    lock(&self.state).pools.get(key).map_or(0, Pool::in_use)
  }

  /// Idle items of `key`.
  pub fn idle_count(&self, key: &K) -> usize {
    lock(&self.state).pools.get(key).map_or(0, Pool::idle_count)
  }

  /// Shuts every key's pool down; see [`Pool::shutdown`]. Later checkouts are interrupted.
  pub fn shutdown(&self) -> Effect<(), Never, HNil> {
    let state = Arc::clone(&self.state);
    suspend(move || {
      let pools: Vec<Pool<A, E, R>> = {
        let mut st = lock(&state);
        st.shut = true;
        st.pools.values().cloned().collect()
      };
      pools
        .iter()
        .fold(succeed(()), |acc, pool| acc.then(pool.shutdown()))
    })
    .uninterruptible()
  }
}
