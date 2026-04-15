//! Effectful memoization with TTL, capacity (LRU), miss coalescing via [`crate::Deferred`].
//!
//! In-flight loads for the same key share one [`Deferred`], stored under
//! [`SynchronizedRef`] together with cached entries (see gap analysis §10).

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::collections::hash_map::{self, EffectHashMap};
use crate::coordination::deferred::Deferred;
use crate::coordination::synchronized_ref::SynchronizedRef;
use crate::failure::cause::Cause;
use crate::kernel::{Effect, box_future};
use crate::runtime::Never;

// ── Stats ───────────────────────────────────────────────────────────────────

/// Hit/miss/eviction/load counters (best-effort; updated under the cache mutex).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CacheStats {
  /// Lookups satisfied from a fresh cached entry.
  pub hits: u64,
  /// Lookups that missed the cache or joined an in-flight load for the key.
  pub misses: u64,
  /// Entries removed to honor LRU capacity.
  pub evictions: u64,
  /// Completed loads that stored a new entry.
  pub loads: u64,
}

// ── State ───────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
struct Cached<V> {
  value: V,
  stored_at: Instant,
}

#[derive(Clone)]
struct CacheState<K, V, E>
where
  K: Clone + Eq + std::hash::Hash,
  V: Clone,
  E: Clone,
{
  entries: EffectHashMap<K, Cached<V>>,
  in_flight: HashMap<K, Deferred<V, E>>,
  lru: VecDeque<K>,
  stats: CacheStats,
}

impl<K, V, E> Default for CacheState<K, V, E>
where
  K: Clone + Eq + std::hash::Hash,
  V: Clone,
  E: Clone,
{
  fn default() -> Self {
    Self {
      entries: hash_map::empty(),
      in_flight: HashMap::new(),
      lru: VecDeque::new(),
      stats: CacheStats::default(),
    }
  }
}

fn is_fresh<V>(ttl: Option<Duration>, c: &Cached<V>) -> bool {
  match ttl {
    None => true,
    Some(t) => c.stored_at.elapsed() <= t,
  }
}

fn touch_lru<K: Clone + Eq>(lru: &mut VecDeque<K>, key: &K) {
  lru.retain(|k| k != key);
  lru.push_back(key.clone());
}

fn remove_lru<K: Eq>(lru: &mut VecDeque<K>, key: &K) {
  lru.retain(|k| k != key);
}

impl<K, V, E> CacheState<K, V, E>
where
  K: Clone + Eq + std::hash::Hash,
  V: Clone,
  E: Clone,
{
  fn evict_lru(&mut self, capacity: usize) {
    if capacity == 0 {
      return;
    }
    while self.entries.len() >= capacity {
      if let Some(victim) = self.lru.pop_front() {
        if self.entries.contains_key(&victim) {
          self.entries = hash_map::remove(&self.entries, &victim);
          self.stats.evictions = self.stats.evictions.saturating_add(1);
        }
        continue;
      }
      // LRU desynced — drop one arbitrary entry.
      if let Some(k) = self.entries.keys().next().cloned() {
        self.entries = hash_map::remove(&self.entries, &k);
        remove_lru(&mut self.lru, &k);
        self.stats.evictions = self.stats.evictions.saturating_add(1);
      } else {
        break;
      }
    }
  }

  fn put_entry(&mut self, capacity: usize, key: K, value: V) {
    let had_key = self.entries.contains_key(&key);
    if !had_key && capacity > 0 && self.entries.len() >= capacity {
      self.evict_lru(capacity);
    }
    remove_lru(&mut self.lru, &key);
    self.entries = hash_map::set(
      &self.entries,
      key.clone(),
      Cached {
        value,
        stored_at: Instant::now(),
      },
    );
    self.lru.push_back(key);
  }
}

// ── Phase ───────────────────────────────────────────────────────────────────

enum Phase1<V, E> {
  Hit(V),
  Join(Deferred<V, E>),
  Miss,
}

enum Phase2<V, E> {
  Hit(V),
  Follow(Deferred<V, E>),
  Leader(Deferred<V, E>),
}

// ── Cache ───────────────────────────────────────────────────────────────────

/// Memoizing cache: one shared load per key in flight; optional TTL and LRU capacity.
#[derive(Clone)]
pub struct Cache<K, V, E, R>
where
  K: Clone + Eq + std::hash::Hash + Send + Sync + 'static,
  V: Clone + Send + Sync + 'static,
  E: Clone + Send + Sync + 'static,
  R: Send + 'static,
{
  capacity: usize,
  ttl: Option<Duration>,
  load: Arc<dyn Fn(K) -> Effect<V, E, R> + Send + Sync>,
  state: SynchronizedRef<CacheState<K, V, E>>,
}

impl<K, V, E, R> Cache<K, V, E, R>
where
  K: Clone + Eq + std::hash::Hash + Send + Sync + 'static,
  V: Clone + Send + Sync + 'static,
  E: Clone + Send + Sync + 'static,
  R: Send + 'static,
{
  /// `capacity == 0` disables LRU eviction (unbounded entries). `ttl: None` disables expiry.
  pub fn make<F>(capacity: usize, ttl: Option<Duration>, load: F) -> Effect<Self, Never, ()>
  where
    F: Fn(K) -> Effect<V, E, R> + Send + Sync + 'static,
  {
    Effect::new_async(move |_r| {
      box_future(async move {
        let state = SynchronizedRef::make(CacheState::default())
          .run(&mut ())
          .await
          .expect("cache state");
        Ok(Cache {
          capacity,
          ttl,
          load: Arc::new(load),
          state,
        })
      })
    })
  }

  /// Look up `key`, coalescing concurrent misses on the same key.
  pub fn get(&self, key: K) -> Effect<V, Cause<E>, R> {
    let state = self.state.clone();
    let load = Arc::clone(&self.load);
    let capacity = self.capacity;
    let ttl = self.ttl;
    let key = key;
    Effect::new_async(move |r| {
      box_future(async move {
        let mut z = ();
        let phase1 = state
          .modify({
            let key = key.clone();
            move |mut s| {
              if let Some(c) = hash_map::get(&s.entries, &key).filter(|c| is_fresh(ttl, c)) {
                s.stats.hits = s.stats.hits.saturating_add(1);
                touch_lru(&mut s.lru, &key);
                return (Phase1::Hit(c.value.clone()), s);
              }
              if let Some(d) = s.in_flight.get(&key).cloned() {
                s.stats.misses = s.stats.misses.saturating_add(1);
                return (Phase1::Join(d), s);
              }
              (Phase1::Miss, s)
            }
          })
          .run(&mut z)
          .await
          .expect("cache phase1");

        match phase1 {
          Phase1::Hit(v) => return Ok(v),
          Phase1::Join(d) => return d.wait().run(&mut z).await,
          Phase1::Miss => {}
        }

        let d_leader = Deferred::make().run(&mut z).await.expect("deferred make");

        let phase2 = state
          .modify({
            let key = key.clone();
            let d_leader = d_leader.clone();
            move |mut s| {
              if let Some(c) = hash_map::get(&s.entries, &key).filter(|c| is_fresh(ttl, c)) {
                s.stats.hits = s.stats.hits.saturating_add(1);
                touch_lru(&mut s.lru, &key);
                return (Phase2::Hit(c.value.clone()), s);
              }
              if let Some(d) = s.in_flight.get(&key).cloned() {
                s.stats.misses = s.stats.misses.saturating_add(1);
                return (Phase2::Follow(d), s);
              }
              s.in_flight.insert(key.clone(), d_leader.clone());
              s.stats.misses = s.stats.misses.saturating_add(1);
              (Phase2::Leader(d_leader), s)
            }
          })
          .run(&mut z)
          .await
          .expect("cache phase2");

        match phase2 {
          Phase2::Hit(v) => Ok(v),
          Phase2::Follow(d) => d.wait().run(&mut z).await,
          Phase2::Leader(d) => {
            let load_eff = load(key.clone());
            match load_eff.run(r).await {
              Ok(v) => {
                let won = d.succeed(v.clone()).run(&mut z).await.expect("succeed");
                state
                  .modify({
                    let key = key.clone();
                    let v2 = v.clone();
                    let cap = capacity;
                    move |mut s| {
                      s.in_flight.remove(&key);
                      if won {
                        s.put_entry(cap, key, v2);
                        s.stats.loads = s.stats.loads.saturating_add(1);
                      }
                      ((), s)
                    }
                  })
                  .run(&mut z)
                  .await
                  .expect("cache commit");
                Ok(v)
              }
              Err(e) => {
                let _ = d.fail(e.clone()).run(&mut z).await;
                state
                  .modify({
                    let key = key.clone();
                    move |mut s| {
                      s.in_flight.remove(&key);
                      ((), s)
                    }
                  })
                  .run(&mut z)
                  .await
                  .expect("cache fail cleanup");
                Err(Cause::fail(e))
              }
            }
          }
        }
      })
    })
  }

  /// Drop a cached entry; interrupt any in-flight load for `key` so waiters observe an interrupt.
  pub fn invalidate(&self, key: K) -> Effect<(), Never, ()> {
    let state = self.state.clone();
    let key = key;
    Effect::new_async(move |_r| {
      box_future(async move {
        let mut z = ();
        let to_interrupt = state
          .modify({
            let key = key;
            move |mut s| {
              s.entries = hash_map::remove(&s.entries, &key);
              remove_lru(&mut s.lru, &key);
              let d = s.in_flight.remove(&key);
              (d, s)
            }
          })
          .run(&mut z)
          .await
          .expect("invalidate modify");

        if let Some(d) = to_interrupt {
          let _ = d.interrupt().run(&mut z).await;
        }
        Ok(())
      })
    })
  }

  /// Snapshot of counters.
  pub fn stats(&self) -> Effect<CacheStats, Never, ()> {
    let state = self.state.clone();
    Effect::new_async(move |_r| {
      box_future(async move {
        let mut z = ();
        let st = state
          .modify(|s| (s.stats, s))
          .run(&mut z)
          .await
          .expect("stats");
        Ok(st)
      })
    })
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::kernel::succeed;
  use crate::runtime::run_async;
  use futures::future::join_all;
  use std::sync::Arc;
  use std::sync::atomic::{AtomicUsize, Ordering};

  #[tokio::test]
  async fn cache_hit_returns_cached_value() {
    let cache = run_async(
      Cache::make(8, None, |k: u8| succeed::<i32, (), ()>(k as i32 * 10)),
      (),
    )
    .await
    .expect("make");
    let mut env = ();
    assert_eq!(cache.get(3).run(&mut env).await.expect("g1"), 30);
    assert_eq!(cache.get(3).run(&mut env).await.expect("g2"), 30);
    let st = cache.stats().run(&mut ()).await.expect("stats");
    assert!(st.hits >= 1);
  }

  #[tokio::test]
  async fn cache_miss_coalesces_concurrent_callers() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls2 = Arc::clone(&calls);
    let cache = run_async(
      Cache::make(8, None, move |k: u32| {
        let c = Arc::clone(&calls2);
        Effect::<i64, (), ()>::new_async(move |_r| {
          box_future(async move {
            c.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(40)).await;
            Ok(k as i64 * 2)
          })
        })
      }),
      (),
    )
    .await
    .expect("make");

    let fs: Vec<_> = (0..12)
      .map(|_| {
        let c = cache.clone();
        async move {
          let mut e = ();
          c.get(7).run(&mut e).await.expect("get")
        }
      })
      .collect();
    let got = join_all(fs).await;
    assert!(got.iter().all(|&v| v == 14));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
  }

  #[tokio::test]
  async fn cache_ttl_evicts_stale_entry() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls2 = Arc::clone(&calls);
    let cache = run_async(
      Cache::make(8, Some(Duration::from_millis(30)), move |k: u8| {
        let c = Arc::clone(&calls2);
        Effect::<i32, (), ()>::new(move |_r| {
          c.fetch_add(1, Ordering::SeqCst);
          Ok(k as i32)
        })
      }),
      (),
    )
    .await
    .expect("make");

    let mut env = ();
    assert_eq!(cache.get(1).run(&mut env).await.expect("a"), 1);
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert_eq!(cache.get(1).run(&mut env).await.expect("b"), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
  }

  #[tokio::test]
  async fn cache_invalidate_forces_reload() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls2 = Arc::clone(&calls);
    let cache = run_async(
      Cache::make(8, None, move |k: u8| {
        let c = Arc::clone(&calls2);
        Effect::<i32, (), ()>::new(move |_r| {
          c.fetch_add(1, Ordering::SeqCst);
          Ok(k as i32 * 100)
        })
      }),
      (),
    )
    .await
    .expect("make");

    let mut env = ();
    assert_eq!(cache.get(5).run(&mut env).await.expect("a"), 500);
    cache.invalidate(5).run(&mut ()).await.expect("inv");
    assert_eq!(cache.get(5).run(&mut env).await.expect("b"), 500);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
  }

  // ── stats: misses / loads / evictions ────────────────────────────────────

  #[tokio::test]
  async fn cache_stats_tracks_misses_and_loads() {
    let cache = run_async(
      Cache::make(8, None, |k: u8| succeed::<i32, (), ()>(k as i32)),
      (),
    )
    .await
    .expect("make");

    let mut env = ();
    let _ = cache.get(1).run(&mut env).await.expect("miss1");
    let _ = cache.get(1).run(&mut env).await.expect("hit1");
    let _ = cache.get(2).run(&mut env).await.expect("miss2");

    let st = cache.stats().run(&mut ()).await.expect("stats");
    assert!(st.misses >= 2, "at least 2 misses (keys 1 and 2)");
    assert!(st.loads >= 2, "at least 2 loads");
    assert!(st.hits >= 1, "at least 1 hit (second get for key 1)");
  }

  #[tokio::test]
  async fn cache_lru_evicts_oldest_entry_when_capacity_exceeded() {
    let cache = run_async(Cache::make(2, None, |k: u8| succeed::<u8, (), ()>(k)), ())
      .await
      .expect("make");

    let mut env = ();
    let _ = cache.get(1).run(&mut env).await.expect("k1");
    let _ = cache.get(2).run(&mut env).await.expect("k2");
    // Inserting key 3 should evict key 1 (LRU)
    let _ = cache.get(3).run(&mut env).await.expect("k3");

    let st = cache.stats().run(&mut ()).await.expect("stats");
    assert!(st.evictions >= 1, "at least one eviction expected");
  }

  // ── load failure path ─────────────────────────────────────────────────────

  #[tokio::test]
  async fn cache_get_propagates_load_failure() {
    let cache = run_async(
      Cache::make(8, None, |_k: u8| {
        crate::kernel::fail::<i32, &'static str, ()>("load_error")
      }),
      (),
    )
    .await
    .expect("make");

    let mut env = ();
    let result = cache.get(42).run(&mut env).await;
    assert!(result.is_err(), "load failure should propagate");
  }
}
