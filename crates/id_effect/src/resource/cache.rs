//! `Cache`: effectful memoization with a lookup, an LRU capacity and a time-to-live on the
//! executor clock (Effect.ts `Cache`).
//!
//! Concurrent misses on one key share a single lookup. The lookup's outcome is cached: a
//! success **and** a typed failure (Effect.ts caches the `Exit`), each for `ttl`. A defect is
//! handed to the waiting callers but not cached. An interrupted lookup is not cached either:
//! the callers that were waiting on it retry, and one of them runs the lookup again.
//!
//! [`Cache::with_backend`] keeps the values in a [`CacheBackend`] instead: another in-process
//! `Cache`, or a shared store such as Redis (`id_effect_redis`). The `Cache` API and its
//! single-flight lookups stay the same, so the same code runs on either.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::hash::Hash;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::gate::{Slot, lock, under_any_env, with_now};
use crate::capability::HNil;
use crate::failure::Never;
use crate::failure::{Cause, Exit};
use crate::kernel::{Effect, ExitKind, Restore, suspend, sync, uninterruptible_mask};

/// Hit and miss counters and the current size of a [`Cache`] (Effect.ts `CacheStats`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct CacheStats {
  /// Lookups answered from a fresh cached entry.
  pub hits: u64,
  /// Lookups that ran the lookup function or joined one in flight.
  pub misses: u64,
  /// Completed entries currently cached.
  pub size: usize,
}

/// A [`CacheBackend`] operation failed: the store is unreachable, or a stored value does not
/// decode.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CacheBackendError(pub String);

impl fmt::Display for CacheBackendError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "cache backend: {}", self.0)
  }
}

impl std::error::Error for CacheBackendError {}

/// Where a [`Cache`] built with [`Cache::with_backend`] keeps its values: an in-process
/// [`Cache`] (which implements this trait), or a store shared by several processes, such as
/// `id_effect_redis::RedisCacheBackend`. Code written against `Cache` runs on either.
///
/// A backend holds successes only, and applies its own expiry and eviction. Its operations are
/// effects, so a backend may do I/O; a `Cache` treats a failed read as a miss and a failed write
/// or invalidation as done, and counts both ([`Cache::backend_errors`]): a cache outage slows
/// callers down, it does not fail them.
pub trait CacheBackend<K, V>: Send + Sync + 'static {
  /// The value stored for `key`, if it is present and fresh.
  fn get(&self, key: &K) -> Effect<Option<V>, CacheBackendError>;

  /// Stores `value` for `key`, replacing the previous value.
  fn set(&self, key: K, value: V) -> Effect<(), CacheBackendError>;

  /// Removes the value for `key`, if any.
  fn invalidate(&self, key: &K) -> Effect<(), CacheBackendError>;

  /// Removes every value.
  fn invalidate_all(&self) -> Effect<(), CacheBackendError>;

  /// How many values are stored (expired ones not yet evicted may count).
  fn size(&self) -> Effect<usize, CacheBackendError>;
}

impl<K, V, T: CacheBackend<K, V> + ?Sized> CacheBackend<K, V> for Arc<T> {
  fn get(&self, key: &K) -> Effect<Option<V>, CacheBackendError> {
    (**self).get(key)
  }

  fn set(&self, key: K, value: V) -> Effect<(), CacheBackendError> {
    (**self).set(key, value)
  }

  fn invalidate(&self, key: &K) -> Effect<(), CacheBackendError> {
    (**self).invalidate(key)
  }

  fn invalidate_all(&self) -> Effect<(), CacheBackendError> {
    (**self).invalidate_all()
  }

  fn size(&self) -> Effect<usize, CacheBackendError> {
    (**self).size()
  }
}

type Backend<K, V> = Arc<dyn CacheBackend<K, V>>;

type LookupFn<K, V, E, R> = Arc<dyn Fn(K) -> Effect<V, E, R> + Send + Sync>;

/// What the callers waiting on an in-flight lookup receive.
#[derive(Clone)]
enum Outcome<V, E> {
  Done(Result<V, Cause<E>>),
  /// The lookup was interrupted: look up again.
  Retry,
}

type Pending<V, E> = Arc<Slot<Outcome<V, E>>>;

enum Entry<V, E> {
  Pending(Pending<V, E>),
  Ready {
    exit: Result<V, Cause<E>>,
    at: Instant,
    tick: u64,
  },
}

struct CacheState<K, V, E> {
  entries: HashMap<K, Entry<V, E>>,
  /// Ready entries by last use (smallest tick = least recently used).
  lru: BTreeMap<u64, K>,
  tick: u64,
  hits: u64,
  misses: u64,
  /// Failed backend operations (with a backend).
  backend_errors: u64,
}

impl<K: Clone + Eq + Hash, V, E> CacheState<K, V, E> {
  fn next_tick(&mut self) -> u64 {
    self.tick += 1;
    self.tick
  }

  fn remove(&mut self, key: &K) {
    if let Some(Entry::Ready { tick, .. }) = self.entries.remove(key) {
      self.lru.remove(&tick);
    }
  }

  fn touch(&mut self, key: &K) {
    let new = self.next_tick();
    if let Some(Entry::Ready { tick, .. }) = self.entries.get_mut(key) {
      let old = std::mem::replace(tick, new);
      self.lru.remove(&old);
      self.lru.insert(new, key.clone());
    }
  }

  /// Stores a ready entry, then evicts least recently used ones beyond `capacity` (`0`:
  /// unbounded).
  fn insert_ready(&mut self, key: K, exit: Result<V, Cause<E>>, at: Instant, capacity: usize) {
    self.remove(&key);
    let tick = self.next_tick();
    self.lru.insert(tick, key.clone());
    self.entries.insert(key, Entry::Ready { exit, at, tick });
    while capacity > 0 && self.lru.len() > capacity {
      match self.lru.pop_first() {
        Some((_, victim)) => {
          self.entries.remove(&victim);
        }
        None => break,
      }
    }
  }
}

enum Step<V, E> {
  Hit(Result<V, Cause<E>>),
  Wait(Pending<V, E>),
  Lead(Pending<V, E>),
}

/// A memoizing cache over an effectful `lookup` (Effect.ts `Cache`).
///
/// Built with a plain constructor; clones share the cache. Entries expire `ttl` after they were
/// stored, measured on the running fiber's executor clock (virtual time under a
/// `TestExecutor`). With a non-zero `capacity`, the least recently used entry is evicted once
/// more than `capacity` entries are cached.
///
/// ```
/// use std::sync::Arc;
/// use std::sync::atomic::{AtomicUsize, Ordering};
/// use id_effect::Exit;
/// use id_effect::{Cache, sync};
///
/// let calls = Arc::new(AtomicUsize::new(0));
/// let c = Arc::clone(&calls);
/// let cache: Cache<u32, u32> = Cache::new(16, None, move |k: u32| {
///   c.fetch_add(1, Ordering::SeqCst);
///   sync(move || k * 10)
/// });
/// let eff = cache.get(4).zip(cache.get(4));
/// assert!(matches!(eff.block_on(), Exit::Success((40, 40))));
/// assert_eq!(calls.load(Ordering::SeqCst), 1);
/// ```
///
/// Built with [`with_backend`](Self::with_backend), the cache keeps its values in a
/// [`CacheBackend`] (an in-process `Cache`, or a shared store such as Redis) and the same
/// methods work on them; concurrent misses on one key in this process still share one lookup.
pub struct Cache<K, V, E = Never, R = HNil> {
  state: Arc<Mutex<CacheState<K, V, E>>>,
  lookup: LookupFn<K, V, E, R>,
  capacity: usize,
  ttl: Option<Duration>,
  /// Where the values live; `None`: in `state` (the in-process cache).
  backend: Option<Backend<K, V>>,
}

impl<K, V, E, R> Clone for Cache<K, V, E, R> {
  fn clone(&self) -> Self {
    Self {
      state: Arc::clone(&self.state),
      lookup: Arc::clone(&self.lookup),
      capacity: self.capacity,
      ttl: self.ttl,
      backend: self.backend.clone(),
    }
  }
}

impl<K, V, E, R> std::fmt::Debug for Cache<K, V, E, R> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    let st = lock(&self.state);
    f.debug_struct("Cache")
      .field("capacity", &self.capacity)
      .field("ttl", &self.ttl)
      .field("entries", &st.entries.len())
      .finish()
  }
}

fn fresh(ttl: Option<Duration>, at: Instant, now: Instant) -> bool {
  ttl.is_none_or(|ttl| now.saturating_duration_since(at) < ttl)
}

impl<K, V, E, R> Cache<K, V, E, R>
where
  K: Clone + Eq + Hash + Send + 'static,
  V: Clone + Send + 'static,
  E: Clone + Send + 'static,
  R: Send + Sync + 'static,
{
  /// A cache of at most `capacity` entries (`0`: unbounded) that expire `ttl` after they were
  /// stored (`None`: never), filled by `lookup`. A plain constructor: nothing is looked up
  /// until the first [`get`](Self::get).
  pub fn new(
    capacity: usize,
    ttl: Option<Duration>,
    lookup: impl Fn(K) -> Effect<V, E, R> + Send + Sync + 'static,
  ) -> Self {
    Self {
      state: Arc::new(Mutex::new(CacheState {
        entries: HashMap::new(),
        lru: BTreeMap::new(),
        tick: 0,
        hits: 0,
        misses: 0,
        backend_errors: 0,
      })),
      lookup: Arc::new(lookup),
      capacity,
      ttl,
      backend: None,
    }
  }

  /// A cache whose values live in `backend`, filled by `lookup`: the same API as
  /// [`new`](Self::new), over an in-process [`Cache`] or a shared store such as
  /// `id_effect_redis::RedisCacheBackend`. The backend applies its own expiry and capacity.
  ///
  /// Differences from the in-process cache: only successes are stored (a typed failure is
  /// returned to the callers of that lookup, not cached), a hit is a value read from the
  /// backend, and a backend failure is a miss (on read) or ignored (on write and
  /// invalidation), counted by [`backend_errors`](Self::backend_errors). A plain constructor:
  /// nothing is read until the first [`get`](Self::get).
  ///
  /// ```
  /// use std::time::Duration;
  /// use id_effect::{Cache, Exit, sync};
  ///
  /// // Any `CacheBackend` fits; here, an in-process cache holding 100 values for a minute.
  /// let store: Cache<u32, String> = Cache::new(100, Some(Duration::from_secs(60)), |k: u32| {
  ///   sync(move || k.to_string())
  /// });
  /// let users: Cache<u32, String> =
  ///   Cache::with_backend(store.clone(), |id: u32| sync(move || format!("user-{id}")));
  /// let eff = users.get(7).zip(users.get(7)).zip(users.stats());
  /// let Exit::Success(((a, b), stats)) = eff.block_on() else { panic!() };
  /// assert_eq!((a.as_str(), b.as_str()), ("user-7", "user-7"));
  /// assert_eq!((stats.hits, stats.misses, stats.size), (1, 1, 1));
  /// ```
  pub fn with_backend(
    backend: impl CacheBackend<K, V>,
    lookup: impl Fn(K) -> Effect<V, E, R> + Send + Sync + 'static,
  ) -> Self {
    let mut cache = Self::new(0, None, lookup);
    cache.backend = Some(Arc::new(backend));
    cache
  }

  /// The value for `key` (Effect.ts `Cache.get`): a fresh cached outcome, else the outcome of
  /// the lookup in flight for `key`, else a new lookup whose outcome is cached.
  ///
  /// Waiting on another caller's lookup is interruptible; interrupting the caller that runs the
  /// lookup interrupts the lookup, and the other callers then retry.
  pub fn get(&self, key: K) -> Effect<V, E, R> {
    let this = self.clone();
    match self.backend.clone() {
      None => {
        uninterruptible_mask(move |restore| with_now(move |now| this.step(key, now, restore)))
      }
      Some(backend) => uninterruptible_mask(move |restore| {
        suspend(move || this.step_backend(backend, key, restore))
      }),
    }
  }

  /// The masked part of [`get`](Self::get) with a backend: join the lookup in flight for `key`,
  /// or lead one (read the backend; on a miss run the lookup and store a success).
  fn step_backend(self, backend: Backend<K, V>, key: K, restore: Restore) -> Effect<V, E, R> {
    let slot = {
      let mut st = lock(&self.state);
      if let Some(Entry::Pending(slot)) = st.entries.get(&key) {
        let slot = Arc::clone(slot);
        st.misses += 1;
        drop(st);
        return restore.apply(slot.wait()).flat_map(move |o| match o {
          Outcome::Done(r) => Effect::pure(r),
          Outcome::Retry => restore.apply(self.get(key)),
        });
      }
      let slot = Slot::new();
      st.entries
        .insert(key.clone(), Entry::Pending(Arc::clone(&slot)));
      slot
    };
    let read = restore
      .apply(self.quiet(backend.get(&key), None))
      .exit::<E>();
    read.flat_map(move |exit| match exit {
      Exit::Success(Some(v)) => {
        lock(&self.state).hits += 1;
        self.finish_backend(backend, key, slot, Exit::Success(v), false)
      }
      Exit::Success(None) => {
        lock(&self.state).misses += 1;
        let lookup = Arc::clone(&self.lookup);
        let k = key.clone();
        restore
          .apply(suspend(move || lookup(k)))
          .exit::<E>()
          .flat_map(move |exit| self.finish_backend(backend, key, slot, exit, true))
      }
      Exit::Failure(c) => self.finish_backend(backend, key, slot, Exit::Failure(c), false),
    })
  }

  /// Writes a looked-up success to the backend (when `store`), then settles the pending entry
  /// like [`finish`](Self::finish). An entry invalidated or replaced while the write ran is
  /// invalidated in the backend again, so the write cannot outlive the invalidation.
  fn finish_backend(
    self,
    backend: Backend<K, V>,
    key: K,
    slot: Pending<V, E>,
    exit: Exit<V, E>,
    store: bool,
  ) -> Effect<V, E, R> {
    let r = match exit {
      Exit::Success(v) => Ok(v),
      Exit::Failure(c) => Err(c),
    };
    let write = match &r {
      Ok(v) if store && self.leads(&key, &slot) => {
        let (this, k, s) = (self.clone(), key.clone(), Arc::clone(&slot));
        self
          .quiet(backend.set(key.clone(), v.clone()), ())
          .flat_map(move |()| {
            if this.leads(&k, &s) {
              Effect::pure(Ok(()))
            } else {
              this.quiet(backend.invalidate(&k), ())
            }
          })
      }
      _ => Effect::pure(Ok(())),
    };
    write.flat_map(move |()| {
      if self.leads(&key, &slot) {
        lock(&self.state).entries.remove(&key);
      }
      slot.complete(match ExitKind::of_result(&r) {
        ExitKind::Interrupt => Outcome::Retry,
        _ => Outcome::Done(r.clone()),
      });
      Effect::pure(r)
    })
  }

  /// `true` while `slot` is still the pending entry of `key` (not invalidated or replaced).
  fn leads(&self, key: &K, slot: &Pending<V, E>) -> bool {
    matches!(lock(&self.state).entries.get(key), Some(Entry::Pending(s)) if Arc::ptr_eq(s, slot))
  }

  /// Runs the backend operation `eff` under any error and env: a failure counts as a backend
  /// error and yields `fallback`.
  fn quiet<A: Send + 'static, E2: Send + 'static, R2: Send + Sync + 'static>(
    &self,
    eff: Effect<A, CacheBackendError>,
    fallback: A,
  ) -> Effect<A, E2, R2> {
    let state = Arc::clone(&self.state);
    under_any_env(eff.catch(move |_| {
      lock(&state).backend_errors += 1;
      Effect::pure(Ok(fallback))
    }))
  }

  /// How many backend operations failed (each was treated as a miss or ignored); always `0`
  /// without a backend.
  pub fn backend_errors<E2: Send + 'static, R2: Send + Sync + 'static>(
    &self,
  ) -> Effect<u64, E2, R2> {
    let state = Arc::clone(&self.state);
    sync(move || lock(&state).backend_errors)
  }

  /// The masked part of [`get`](Self::get).
  fn step(self, key: K, now: Instant, restore: Restore) -> Effect<V, E, R> {
    match self.begin(&key, now) {
      Step::Hit(r) => Effect::pure(r),
      Step::Wait(slot) => restore.apply(slot.wait()).flat_map(move |o| match o {
        Outcome::Done(r) => Effect::pure(r),
        Outcome::Retry => restore.apply(self.get(key)),
      }),
      Step::Lead(slot) => {
        let lookup = Arc::clone(&self.lookup);
        let k = key.clone();
        restore
          .apply(suspend(move || lookup(k)))
          .exit::<E>()
          .flat_map(move |exit| with_now(move |now| self.finish(key, &slot, exit, now)))
      }
    }
  }

  /// Classifies a lookup of `key`, registering a pending entry when this caller leads.
  fn begin(&self, key: &K, now: Instant) -> Step<V, E> {
    let mut st = lock(&self.state);
    let found = match st.entries.get(key) {
      Some(Entry::Pending(slot)) => Some(Step::Wait(Arc::clone(slot))),
      Some(Entry::Ready { exit, at, .. }) if fresh(self.ttl, *at, now) => {
        Some(Step::Hit(exit.clone()))
      }
      Some(Entry::Ready { .. }) => {
        st.remove(key);
        None
      }
      None => None,
    };
    match found {
      Some(Step::Hit(r)) => {
        st.hits += 1;
        st.touch(key);
        Step::Hit(r)
      }
      Some(step) => {
        st.misses += 1;
        step
      }
      None => {
        st.misses += 1;
        let slot = Slot::new();
        st.entries
          .insert(key.clone(), Entry::Pending(Arc::clone(&slot)));
        Step::Lead(slot)
      }
    }
  }

  /// Stores the leader's outcome (unless the entry was invalidated or replaced meanwhile),
  /// wakes the waiters, and re-raises it.
  fn finish(self, key: K, slot: &Pending<V, E>, exit: Exit<V, E>, now: Instant) -> Effect<V, E, R> {
    let r = match exit {
      Exit::Success(v) => Ok(v),
      Exit::Failure(c) => Err(c),
    };
    let kind = ExitKind::of_result(&r);
    {
      let mut st = lock(&self.state);
      let ours = matches!(st.entries.get(&key), Some(Entry::Pending(s)) if Arc::ptr_eq(s, slot));
      if ours {
        st.entries.remove(&key);
        if matches!(kind, ExitKind::Success | ExitKind::Failure) {
          st.insert_ready(key, r.clone(), now, self.capacity);
        }
      }
    }
    slot.complete(match kind {
      ExitKind::Interrupt => Outcome::Retry,
      _ => Outcome::Done(r.clone()),
    });
    Effect::pure(r)
  }

  /// The fresh cached success for `key`, without looking it up or waiting for a lookup in
  /// flight (Effect.ts `Cache.getOptionComplete`).
  pub fn get_if_present<E2: Send + 'static, R2: Send + Sync + 'static>(
    &self,
    key: &K,
  ) -> Effect<Option<V>, E2, R2> {
    if let Some(backend) = &self.backend {
      return self.quiet(backend.get(key), None);
    }
    let this = self.clone();
    let key = key.clone();
    with_now(move |now| {
      let st = lock(&this.state);
      let v = match st.entries.get(&key) {
        Some(Entry::Ready {
          exit: Ok(v), at, ..
        }) if fresh(this.ttl, *at, now) => Some(v.clone()),
        _ => None,
      };
      Effect::pure(Ok(v))
    })
  }

  /// `true` when `key` has a fresh cached outcome or a lookup in flight (Effect.ts
  /// `Cache.contains`).
  pub fn contains<E2: Send + 'static, R2: Send + Sync + 'static>(
    &self,
    key: &K,
  ) -> Effect<bool, E2, R2> {
    if let Some(backend) = self.backend.clone() {
      let this = self.clone();
      let key = key.clone();
      return suspend(move || {
        let pending = matches!(lock(&this.state).entries.get(&key), Some(Entry::Pending(_)));
        if pending {
          Effect::pure(Ok(true))
        } else {
          this.quiet(backend.get(&key), None).map(|v| v.is_some())
        }
      });
    }
    let this = self.clone();
    let key = key.clone();
    with_now(move |now| {
      let st = lock(&this.state);
      let present = match st.entries.get(&key) {
        Some(Entry::Pending(_)) => true,
        Some(Entry::Ready { at, .. }) => fresh(this.ttl, *at, now),
        None => false,
      };
      Effect::pure(Ok(present))
    })
  }

  /// Caches `value` for `key` (Effect.ts `Cache.set`). A lookup in flight for `key` still
  /// answers its own callers, but its outcome is not stored over `value`.
  pub fn set<E2: Send + 'static, R2: Send + Sync + 'static>(
    &self,
    key: K,
    value: V,
  ) -> Effect<(), E2, R2> {
    if let Some(backend) = self.backend.clone() {
      let this = self.clone();
      return suspend(move || {
        lock(&this.state).entries.remove(&key);
        this.quiet(backend.set(key, value), ())
      });
    }
    let this = self.clone();
    with_now(move |now| {
      lock(&this.state).insert_ready(key, Ok(value), now, this.capacity);
      Effect::pure(Ok(()))
    })
  }

  /// Runs the lookup for `key` again and caches a success over the current entry, which keeps
  /// answering [`get`](Self::get) meanwhile (Effect.ts `Cache.refresh`). A failure is returned
  /// and leaves the current entry in place.
  pub fn refresh(&self, key: K) -> Effect<(), E, R> {
    let this = self.clone();
    let lookup = Arc::clone(&self.lookup);
    let k = key.clone();
    suspend(move || lookup(k)).flat_map(move |v| this.set(key, v))
  }

  /// Drops the entry for `key` (Effect.ts `Cache.invalidate`). A lookup in flight still answers
  /// its callers, but its outcome is not stored.
  pub fn invalidate<E2: Send + 'static, R2: Send + Sync + 'static>(
    &self,
    key: &K,
  ) -> Effect<(), E2, R2> {
    if let Some(backend) = self.backend.clone() {
      let this = self.clone();
      let key = key.clone();
      return suspend(move || {
        lock(&this.state).entries.remove(&key);
        this.quiet(backend.invalidate(&key), ())
      });
    }
    let state = Arc::clone(&self.state);
    let key = key.clone();
    sync(move || lock(&state).remove(&key))
  }

  /// Drops every entry (Effect.ts `Cache.invalidateAll`).
  pub fn invalidate_all<E2: Send + 'static, R2: Send + Sync + 'static>(
    &self,
  ) -> Effect<(), E2, R2> {
    if let Some(backend) = self.backend.clone() {
      let this = self.clone();
      return suspend(move || {
        lock(&this.state).entries.clear();
        this.quiet(backend.invalidate_all(), ())
      });
    }
    let state = Arc::clone(&self.state);
    sync(move || {
      let mut st = lock(&state);
      st.entries.clear();
      st.lru.clear();
    })
  }

  /// The number of completed entries cached (expired ones not yet evicted included); with a
  /// backend, its [`size`](CacheBackend::size) (`0` when that fails).
  pub fn size<E2: Send + 'static, R2: Send + Sync + 'static>(&self) -> Effect<usize, E2, R2> {
    if let Some(backend) = &self.backend {
      return self.quiet(backend.size(), 0);
    }
    let state = Arc::clone(&self.state);
    sync(move || lock(&state).lru.len())
  }

  /// A snapshot of the hit/miss counters and the size (Effect.ts `Cache.cacheStats`).
  pub fn stats<E2: Send + 'static, R2: Send + Sync + 'static>(&self) -> Effect<CacheStats, E2, R2> {
    let state = Arc::clone(&self.state);
    if self.backend.is_some() {
      return self.size().map(move |size| {
        let st = lock(&state);
        CacheStats {
          hits: st.hits,
          misses: st.misses,
          size,
        }
      });
    }
    sync(move || {
      let st = lock(&state);
      CacheStats {
        hits: st.hits,
        misses: st.misses,
        size: st.lru.len(),
      }
    })
  }
}

/// The in-process cache as the [`CacheBackend`] of another [`Cache`]: its stored entries, under
/// its own capacity and time-to-live (its lookup is not used). Its operations do not fail.
impl<K, V, E, R> CacheBackend<K, V> for Cache<K, V, E, R>
where
  K: Clone + Eq + Hash + Send + 'static,
  V: Clone + Send + 'static,
  E: Clone + Send + 'static,
  R: Send + Sync + 'static,
{
  fn get(&self, key: &K) -> Effect<Option<V>, CacheBackendError> {
    self.get_if_present(key)
  }

  fn set(&self, key: K, value: V) -> Effect<(), CacheBackendError> {
    Cache::set(self, key, value)
  }

  fn invalidate(&self, key: &K) -> Effect<(), CacheBackendError> {
    Cache::invalidate(self, key)
  }

  fn invalidate_all(&self) -> Effect<(), CacheBackendError> {
    Cache::invalidate_all(self)
  }

  fn size(&self) -> Effect<usize, CacheBackendError> {
    Cache::size(self)
  }
}

#[cfg(test)]
mod backend_tests {
  use std::sync::atomic::{AtomicUsize, Ordering};

  use super::*;
  use crate::{Concurrency, die, fail, fork_all, sleep};

  type Store = Cache<u32, u32>;

  fn store() -> Store {
    Cache::new(16, None, |k: u32| sync(move || k))
  }

  fn counted(calls: &Arc<AtomicUsize>) -> impl Fn(u32) -> Effect<u32> + Send + Sync + 'static {
    let calls = Arc::clone(calls);
    move |k| {
      calls.fetch_add(1, Ordering::SeqCst);
      sync(move || k * 10)
    }
  }

  fn ok<A: Send + 'static, E: Send + std::fmt::Debug + 'static>(eff: Effect<A, E>) -> A {
    eff.block_on().into_result().expect("the effect succeeds")
  }

  /// Every operation fails.
  struct Down;

  impl CacheBackend<u32, u32> for Down {
    fn get(&self, _: &u32) -> Effect<Option<u32>, CacheBackendError> {
      crate::fail(CacheBackendError("down".into()))
    }
    fn set(&self, _: u32, _: u32) -> Effect<(), CacheBackendError> {
      crate::fail(CacheBackendError("down".into()))
    }
    fn invalidate(&self, _: &u32) -> Effect<(), CacheBackendError> {
      crate::fail(CacheBackendError("down".into()))
    }
    fn invalidate_all(&self) -> Effect<(), CacheBackendError> {
      crate::fail(CacheBackendError("down".into()))
    }
    fn size(&self) -> Effect<usize, CacheBackendError> {
      crate::fail(CacheBackendError("down".into()))
    }
  }

  /// `inner`, with reads and writes that take a while.
  struct Slow {
    inner: Store,
    read: Duration,
    write: Duration,
  }

  impl CacheBackend<u32, u32> for Slow {
    fn get(&self, key: &u32) -> Effect<Option<u32>, CacheBackendError> {
      let (inner, key) = (self.inner.clone(), *key);
      sleep(self.read).flat_map(move |()| CacheBackend::get(&inner, &key))
    }
    fn set(&self, key: u32, value: u32) -> Effect<(), CacheBackendError> {
      let inner = self.inner.clone();
      sleep(self.write).flat_map(move |()| CacheBackend::set(&inner, key, value))
    }
    fn invalidate(&self, key: &u32) -> Effect<(), CacheBackendError> {
      CacheBackend::invalidate(&self.inner, key)
    }
    fn invalidate_all(&self) -> Effect<(), CacheBackendError> {
      CacheBackend::invalidate_all(&self.inner)
    }
    fn size(&self) -> Effect<usize, CacheBackendError> {
      CacheBackend::size(&self.inner)
    }
  }

  #[test]
  fn values_live_in_the_backend_and_the_api_is_unchanged() {
    let calls = Arc::new(AtomicUsize::new(0));
    let backend = store();
    let cache: Cache<u32, u32> = Cache::with_backend(backend.clone(), counted(&calls));
    assert_eq!(ok(cache.get(1).zip(cache.get(1))), (10, 10));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(ok(backend.get_if_present::<Never, HNil>(&1)), Some(10));
    assert_eq!(
      ok(cache.stats::<Never, HNil>()),
      CacheStats {
        hits: 1,
        misses: 1,
        size: 1
      }
    );
    assert_eq!(ok(cache.get_if_present::<Never, HNil>(&1)), Some(10));
    assert!(ok(cache.contains::<Never, HNil>(&1)));
    assert!(!ok(cache.contains::<Never, HNil>(&2)));

    ok(cache.set::<Never, HNil>(2, 7));
    assert_eq!(ok(cache.get(2)), 7);
    ok(cache.refresh(2));
    assert_eq!(ok(backend.get_if_present::<Never, HNil>(&2)), Some(20));
    ok(cache.invalidate::<Never, HNil>(&1));
    assert_eq!(ok(backend.get_if_present::<Never, HNil>(&1)), None);
    assert_eq!(ok(cache.size::<Never, HNil>()), 1);
    ok(cache.invalidate_all::<Never, HNil>());
    assert_eq!(ok(cache.size::<Never, HNil>()), 0);
    assert_eq!(ok(cache.backend_errors::<Never, HNil>()), 0);
  }

  #[test]
  fn an_arc_and_a_backend_backed_cache_are_backends_too() {
    let shared = Arc::new(store());
    let middle: Cache<u32, u32> = Cache::with_backend(Arc::clone(&shared), |k| sync(move || k));
    let cache: Cache<u32, u32> = Cache::with_backend(middle, |k| sync(move || k + 1));
    assert_eq!(ok(cache.get(4)), 5);
    assert_eq!(ok(shared.get_if_present::<Never, HNil>(&4)), Some(5));
    ok(cache.invalidate_all::<Never, HNil>());
    assert_eq!(ok(CacheBackend::size(&shared)), 0);
  }

  #[test]
  fn an_unavailable_backend_degrades_to_the_lookup_and_is_counted() {
    let calls = Arc::new(AtomicUsize::new(0));
    let cache: Cache<u32, u32> = Cache::with_backend(Down, counted(&calls));
    assert_eq!(ok(cache.get(3).zip(cache.get(3))), (30, 30));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(ok(cache.get_if_present::<Never, HNil>(&3)), None);
    assert!(!ok(cache.contains::<Never, HNil>(&3)));
    ok(cache.set::<Never, HNil>(3, 1));
    ok(cache.invalidate::<Never, HNil>(&3));
    ok(cache.invalidate_all::<Never, HNil>());
    let stats = ok(cache.stats::<Never, HNil>());
    assert_eq!((stats.hits, stats.misses, stats.size), (0, 2, 0));
    // 2 × (read + write), contains, get_if_present, set, invalidate, invalidate_all, size.
    assert_eq!(ok(cache.backend_errors::<Never, HNil>()), 10);
    let e = CacheBackendError("down".into());
    assert_eq!(e.to_string(), "cache backend: down");
  }

  #[test]
  fn failures_and_defects_are_returned_not_stored() {
    let calls = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&calls);
    let backend = store();
    let cache: Cache<u32, u32, String> =
      Cache::with_backend(backend.clone(), move |k: u32| {
        match c.fetch_add(1, Ordering::SeqCst) {
          0 => fail(format!("no {k}")),
          1 => die("boom"),
          _ => sync(move || k),
        }
      });
    assert!(matches!(cache.get(9).block_on(), Exit::Failure(Cause::Fail(m)) if m == "no 9"));
    assert!(matches!(
      cache.get(9).block_on(),
      Exit::Failure(Cause::Die(_))
    ));
    assert_eq!(ok(backend.get_if_present::<Never, HNil>(&9)), None);
    assert!(matches!(cache.get(9).block_on(), Exit::Success(9)));
    assert_eq!(calls.load(Ordering::SeqCst), 3);
  }

  #[test]
  fn concurrent_misses_share_one_lookup_over_a_backend() {
    let calls = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&calls);
    let cache: Cache<u32, u32> = Cache::with_backend(store(), move |k: u32| {
      c.fetch_add(1, Ordering::SeqCst);
      sleep(Duration::from_millis(50)).map(move |()| k + 1)
    });
    let mut effs: Vec<Effect<u32>> = (0..8).map(|_| cache.get(41)).collect();
    // A lookup in flight is contained.
    let pending = cache.contains::<Never, HNil>(&41);
    effs.push(sleep(Duration::from_millis(10)).then(pending.map(u32::from)));
    let vs = ok(fork_all(effs, Concurrency::Unbounded));
    assert_eq!(vs, [vec![42; 8], vec![1]].concat());
    let stats = ok(cache.stats::<Never, HNil>());
    assert_eq!((stats.hits, stats.misses, stats.size), (0, 8, 1));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
  }

  #[test]
  fn an_invalidation_during_the_write_wins() {
    let inner = store();
    let backend = Slow {
      inner: inner.clone(),
      read: Duration::ZERO,
      write: Duration::from_millis(60),
    };
    let cache: Cache<u32, u32> = Cache::with_backend(backend, |k: u32| sync(move || k));
    let get = cache.get(5);
    let invalidate = sleep(Duration::from_millis(20)).then(cache.invalidate::<Never, HNil>(&5));
    let both = fork_all(vec![get, invalidate.map(|()| 0)], Concurrency::Unbounded);
    assert_eq!(ok(both), vec![5, 0]);
    assert_eq!(ok(inner.get_if_present::<Never, HNil>(&5)), None);
    ok(cache.set::<Never, HNil>(6, 6));
    assert_eq!(ok(cache.size::<Never, HNil>()), 1);
    ok(cache.invalidate_all::<Never, HNil>());
    assert_eq!(ok(cache.size::<Never, HNil>()), 0);
    assert!(format!("{cache:?}").starts_with("Cache"));
  }

  #[test]
  fn interrupting_the_reader_makes_the_waiters_retry() {
    let calls = Arc::new(AtomicUsize::new(0));
    let backend = Slow {
      inner: store(),
      read: Duration::from_millis(40),
      write: Duration::ZERO,
    };
    let cache: Cache<u32, u32> = Cache::with_backend(backend, counted(&calls));
    let leader = cache.get(1).timeout(Duration::from_millis(15));
    let waiter = sleep(Duration::from_millis(5)).then(cache.get(1).map(Some));
    let vs = ok(fork_all(vec![leader, waiter], Concurrency::Unbounded));
    assert_eq!(vs, vec![None, Some(10)]);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
  }
}
