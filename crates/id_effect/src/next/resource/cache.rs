//! `Cache`: effectful memoization with a lookup, an LRU capacity and a time-to-live on the
//! executor clock (Effect.ts `Cache`).
//!
//! Concurrent misses on one key share a single lookup. The lookup's outcome is cached: a
//! success **and** a typed failure (Effect.ts caches the `Exit`), each for `ttl`. A defect is
//! handed to the waiting callers but not cached. An interrupted lookup is not cached either:
//! the callers that were waiting on it retry, and one of them runs the lookup again.

use std::collections::{BTreeMap, HashMap};
use std::hash::Hash;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::gate::{Slot, lock, with_now};
use crate::failure::{Cause, Exit};
use crate::next::capability::HNil;
use crate::next::kernel::{Effect, ExitKind, Restore, suspend, sync, uninterruptible_mask};
use crate::runtime::Never;

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
/// use id_effect::next::{Cache, sync};
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
pub struct Cache<K, V, E = Never, R = HNil> {
  state: Arc<Mutex<CacheState<K, V, E>>>,
  lookup: LookupFn<K, V, E, R>,
  capacity: usize,
  ttl: Option<Duration>,
}

impl<K, V, E, R> Clone for Cache<K, V, E, R> {
  fn clone(&self) -> Self {
    Self {
      state: Arc::clone(&self.state),
      lookup: Arc::clone(&self.lookup),
      capacity: self.capacity,
      ttl: self.ttl,
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
      })),
      lookup: Arc::new(lookup),
      capacity,
      ttl,
    }
  }

  /// The value for `key` (Effect.ts `Cache.get`): a fresh cached outcome, else the outcome of
  /// the lookup in flight for `key`, else a new lookup whose outcome is cached.
  ///
  /// Waiting on another caller's lookup is interruptible; interrupting the caller that runs the
  /// lookup interrupts the lookup, and the other callers then retry.
  pub fn get(&self, key: K) -> Effect<V, E, R> {
    let this = self.clone();
    uninterruptible_mask(move |restore| with_now(move |now| this.step(key, now, restore)))
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
    let state = Arc::clone(&self.state);
    let key = key.clone();
    sync(move || lock(&state).remove(&key))
  }

  /// Drops every entry (Effect.ts `Cache.invalidateAll`).
  pub fn invalidate_all<E2: Send + 'static, R2: Send + Sync + 'static>(
    &self,
  ) -> Effect<(), E2, R2> {
    let state = Arc::clone(&self.state);
    sync(move || {
      let mut st = lock(&state);
      st.entries.clear();
      st.lru.clear();
    })
  }

  /// The number of completed entries cached (expired ones not yet evicted included).
  pub fn size<E2: Send + 'static, R2: Send + Sync + 'static>(&self) -> Effect<usize, E2, R2> {
    let state = Arc::clone(&self.state);
    sync(move || lock(&state).lru.len())
  }

  /// A snapshot of the hit/miss counters and the size (Effect.ts `Cache.cacheStats`).
  pub fn stats<E2: Send + 'static, R2: Send + Sync + 'static>(&self) -> Effect<CacheStats, E2, R2> {
    let state = Arc::clone(&self.state);
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
