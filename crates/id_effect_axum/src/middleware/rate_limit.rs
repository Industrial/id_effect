//! HTTP rate limiting: [`RateLimitLayer`] takes one token per request from the bucket of the
//! request's [`RateLimitKey`] and answers `429 Too Many Requests` (Problem, `Retry-After`) when it
//! is empty.
//!
//! Buckets live in a [`RateLimitStore`]. The admission check is an effect run on the effect
//! runtime, so a store can do I/O: [`MemoryRateLimitStore`] keeps one token bucket per key in
//! process (and answers inline, through [`RateLimitStore::try_acquire_now`], without the effect
//! runtime); a shared store (e.g. Redis, for several instances) implements the same trait. A
//! store that dies or is interrupted lets the request through (fail open): the limiter protects
//! capacity, it is not an access control.
//!
//! The default key is the client IP: the connection's peer (`ConnectInfo<SocketAddr>`, which
//! [`serve`](crate::serve) and the other server entry points record on every request, as does
//! `into_make_service_with_connect_info::<SocketAddr>()`), falling back to one bucket shared by
//! every request without it. Behind reverse proxies, key by [`RateLimitKey::client_ip`] with the
//! [`TrustedProxies`] in front of the service: `X-Forwarded-For` is read from the right, so a
//! client cannot pick its own bucket. Per-tenant limits key by the request's tenant with
//! [`RateLimitKey::tenant`] (read from a [`RequestCaps`] value or a request extension the
//! authentication layer recorded). Key by principal with [`RateLimitKey::from_fn`]:
//!
//! ```rust
//! use axum::{Router, routing::get};
//! use id_effect_axum::middleware::{MemoryRateLimitStore, RateLimitKey, RateLimitLayer};
//!
//! #[derive(Clone)]
//! struct Principal(String);
//!
//! let by_user = RateLimitKey::from_fn(|req| {
//!   req.extensions().get::<Principal>().map(|p| format!("user:{}", p.0))
//! });
//! let app: Router = Router::new()
//!   .route("/api", get(|| async { "ok" }))
//!   .layer(
//!     RateLimitLayer::new(MemoryRateLimitStore::new(10.0, 20.0))
//!       .key(by_user.or(RateLimitKey::peer_ip())),
//!   );
//! # let _ = app;
//! ```

use std::collections::HashMap;
use std::convert::Infallible;
use std::fmt;
use std::future::Future;
use std::hash::{BuildHasher, RandomState};
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use axum::extract::Request;
use axum::http::{HeaderName, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use id_effect::{Cause, Ctx, Effect, Exit, HNil};
use tower_layer::Layer;
use tower_service::Service;

pub use id_effect_resilience::{RateLimitError, RateLimiter};

use super::problem_response;
use crate::client_ip::{ClientIp, TrustedProxies};
use crate::executor::EffectExecutor;
use crate::request_caps::RequestCaps;

/// Where [`RateLimitLayer`] keeps its buckets.
pub trait RateLimitStore: Send + Sync + 'static {
  /// Take one token from `key`'s bucket, or fail with [`RateLimitError`] when it is empty.
  fn acquire(&self, key: &str) -> Effect<(), RateLimitError, HNil>;

  /// [`Self::acquire`] without an effect, for a store whose check needs no I/O: `Some(outcome)`
  /// of taking the token now (wall clock), or `None` (the default) to run [`Self::acquire`].
  /// [`RateLimitLayer`] tries it first, so an in-process store skips the executor round trip.
  fn try_acquire_now(&self, _key: &str) -> Option<Result<(), RateLimitError>> {
    None
  }
}

impl<T: RateLimitStore + ?Sized> RateLimitStore for Arc<T> {
  fn acquire(&self, key: &str) -> Effect<(), RateLimitError, HNil> {
    (**self).acquire(key)
  }

  fn try_acquire_now(&self, key: &str) -> Option<Result<(), RateLimitError>> {
    (**self).try_acquire_now(key)
  }
}

/// In-process store: one token bucket per key (`rate` tokens/second, burst `burst`, the
/// arithmetic of [`RateLimiter`]), at most [`Self::max_keys`] keys (the least recently used
/// bucket is dropped first).
///
/// The keys are split over [`Self::shards`] shards by hash, each behind its own mutex, so
/// requests for different keys rarely contend. Each shard keeps its buckets in recency order
/// (a linked list over a slab), so a lookup and an eviction are O(1): a full store drops the
/// least recently used bucket of the new key's shard without looking at the others.
///
/// [`RateLimitLayer`] checks it inline ([`RateLimitStore::try_acquire_now`], wall clock);
/// [`RateLimitStore::acquire`] refills on the running fiber's executor clock.
pub struct MemoryRateLimitStore {
  max_keys: usize,
  shard_count: usize,
  buckets: Arc<Buckets>,
}

/// The shared part of a [`MemoryRateLimitStore`] (an [`RateLimitStore::acquire`] effect holds it).
struct Buckets {
  rate: f64,
  burst: f64,
  hasher: RandomState,
  shards: Box<[Mutex<LruShard>]>,
}

impl Buckets {
  fn shard(&self, key: &str) -> &Mutex<LruShard> {
    let len = self.shards.len() as u64;
    // `len` fits in `usize`, so the remainder does too.
    let i = usize::try_from(self.hasher.hash_one(key) % len).unwrap_or(0);
    &self.shards[i]
  }

  /// Takes one token from `key`'s bucket at `now`, creating the bucket (full; evicting the least
  /// recently used one of its shard when that is full) if missing.
  fn take(&self, key: &str, now: Instant) -> Result<(), RateLimitError> {
    let mut shard = lock(self.shard(key));
    if shard
      .bucket(key, self.burst)
      .take(now, self.rate, self.burst)
    {
      Ok(())
    } else {
      Err(RateLimitError)
    }
  }
}

impl fmt::Debug for MemoryRateLimitStore {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("MemoryRateLimitStore")
      .field("rate", &self.buckets.rate)
      .field("burst", &self.buckets.burst)
      .field("max_keys", &self.max_keys)
      .field("shards", &self.buckets.shards.len())
      .field("keys", &self.len())
      .finish()
  }
}

impl MemoryRateLimitStore {
  /// Default [`Self::max_keys`].
  pub const DEFAULT_MAX_KEYS: usize = 100_000;

  /// Most shards of the default [`Self::shards`]: 64.
  pub const MAX_DEFAULT_SHARDS: usize = 64;

  /// Buckets refilling `rate` tokens per second up to `burst` tokens (each starts full).
  pub fn new(rate: f64, burst: f64) -> Self {
    Self {
      max_keys: Self::DEFAULT_MAX_KEYS,
      shard_count: Self::default_shards(),
      buckets: Arc::new(Buckets {
        rate,
        burst,
        hasher: RandomState::new(),
        shards: Box::new([]),
      }),
    }
    .reshard()
  }

  /// Four shards per available CPU, rounded up to a power of two, at most
  /// [`Self::MAX_DEFAULT_SHARDS`].
  pub fn default_shards() -> usize {
    let cpus = std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get);
    cpus
      .saturating_mul(4)
      .next_power_of_two()
      .min(Self::MAX_DEFAULT_SHARDS)
  }

  /// Keep at most `n` (at least 1) buckets.
  #[must_use]
  pub fn max_keys(mut self, n: usize) -> Self {
    self.max_keys = n.max(1);
    self.reshard()
  }

  /// Split the keys over `n` (at least 1, at most [`Self::max_keys`]) shards; `1` makes the
  /// eviction order exactly least recently used across all keys.
  #[must_use]
  pub fn shards(mut self, n: usize) -> Self {
    self.shard_count = n.max(1);
    self.reshard()
  }

  /// Buckets currently held.
  pub fn len(&self) -> usize {
    self
      .buckets
      .shards
      .iter()
      .map(|shard| lock(shard).index.len())
      .sum()
  }

  /// Whether no bucket is held.
  pub fn is_empty(&self) -> bool {
    self.len() == 0
  }

  /// Empty shards for the current settings: the `max_keys` split as evenly as possible.
  fn reshard(mut self) -> Self {
    let count = self.shard_count.min(self.max_keys);
    let (each, extra) = (self.max_keys / count, self.max_keys % count);
    self.buckets = Arc::new(Buckets {
      rate: self.buckets.rate,
      burst: self.buckets.burst,
      hasher: self.buckets.hasher.clone(),
      shards: (0..count)
        .map(|i| Mutex::new(LruShard::new(each + usize::from(i < extra))))
        .collect(),
    });
    self
  }

  /// Touches `key`'s bucket (taking a token).
  #[cfg(test)]
  fn bucket(&self, key: &str) {
    let _ = self.buckets.take(key, Instant::now());
  }

  #[cfg(test)]
  fn keys(&self) -> Vec<String> {
    let mut keys: Vec<String> = self
      .buckets
      .shards
      .iter()
      .flat_map(|shard| {
        lock(shard)
          .index
          .keys()
          .map(|k| k.to_string())
          .collect::<Vec<_>>()
      })
      .collect();
    keys.sort();
    keys
  }

  #[cfg(test)]
  fn eviction_visits(&self) -> usize {
    self
      .buckets
      .shards
      .iter()
      .map(|shard| lock(shard).visits)
      .sum()
  }
}

fn lock(shard: &Mutex<LruShard>) -> MutexGuard<'_, LruShard> {
  shard.lock().unwrap_or_else(PoisonError::into_inner)
}

/// No slot (the end of the recency list).
const NIL: usize = usize::MAX;

/// A token bucket: `tokens` left, refilled since `last` (unset until the first take).
#[derive(Clone, Copy, Debug)]
struct Bucket {
  tokens: f64,
  last: Option<Instant>,
}

impl Bucket {
  fn full(burst: f64) -> Self {
    Self {
      tokens: burst,
      last: None,
    }
  }

  /// Refills up to `burst` at `rate` tokens per second until `now`, then takes one token;
  /// `false` when less than one is left (as [`RateLimiter::acquire`]).
  fn take(&mut self, now: Instant, rate: f64, burst: f64) -> bool {
    let tokens = match self.last {
      Some(last) => {
        (self.tokens + now.saturating_duration_since(last).as_secs_f64() * rate).min(burst)
      }
      None => self.tokens,
    };
    self.last = Some(now);
    if tokens >= 1.0 {
      self.tokens = tokens - 1.0;
      true
    } else {
      self.tokens = tokens;
      false
    }
  }
}

/// One bucket in a shard's recency list.
struct Node {
  key: Arc<str>,
  bucket: Bucket,
  /// The next more recently used slot.
  newer: usize,
  /// The next less recently used slot.
  older: usize,
}

/// One shard of a [`MemoryRateLimitStore`]: at most `capacity` buckets in a slab, linked from the
/// most recently used (`newest`) to the least (`oldest`), with an index from key to slot.
struct LruShard {
  capacity: usize,
  index: HashMap<Arc<str>, usize>,
  nodes: Vec<Node>,
  newest: usize,
  oldest: usize,
  /// Slots looked at to pick an eviction victim (the O(1) eviction test).
  #[cfg(test)]
  visits: usize,
}

impl LruShard {
  fn new(capacity: usize) -> Self {
    Self {
      capacity: capacity.max(1),
      index: HashMap::new(),
      nodes: Vec::new(),
      newest: NIL,
      oldest: NIL,
      #[cfg(test)]
      visits: 0,
    }
  }

  /// The bucket of `key`, now the most recently used; a missing key gets a full bucket of
  /// `burst`, in a new slot or, when the shard is full, in the least recently used one.
  fn bucket(&mut self, key: &str, burst: f64) -> &mut Bucket {
    if let Some(&slot) = self.index.get(key) {
      if slot != self.newest {
        self.unlink(slot);
        self.push_newest(slot);
      }
      return &mut self.nodes[slot].bucket;
    }
    let key: Arc<str> = Arc::from(key);
    let slot = if self.nodes.len() < self.capacity {
      self.nodes.push(Node {
        key: Arc::clone(&key),
        bucket: Bucket::full(burst),
        newer: NIL,
        older: NIL,
      });
      self.nodes.len() - 1
    } else {
      let slot = self.oldest;
      #[cfg(test)]
      {
        self.visits += 1;
      }
      self.unlink(slot);
      let node = &mut self.nodes[slot];
      let evicted = std::mem::replace(&mut node.key, Arc::clone(&key));
      node.bucket = Bucket::full(burst);
      self.index.remove(&evicted);
      slot
    };
    self.push_newest(slot);
    self.index.insert(key, slot);
    &mut self.nodes[slot].bucket
  }

  fn unlink(&mut self, slot: usize) {
    let (newer, older) = (self.nodes[slot].newer, self.nodes[slot].older);
    match newer {
      NIL => self.newest = older,
      n => self.nodes[n].older = older,
    }
    match older {
      NIL => self.oldest = newer,
      o => self.nodes[o].newer = newer,
    }
  }

  fn push_newest(&mut self, slot: usize) {
    let previous = self.newest;
    self.nodes[slot].newer = NIL;
    self.nodes[slot].older = previous;
    match previous {
      NIL => self.oldest = slot,
      p => self.nodes[p].newer = slot,
    }
    self.newest = slot;
  }
}

impl RateLimitStore for MemoryRateLimitStore {
  fn acquire(&self, key: &str) -> Effect<(), RateLimitError, HNil> {
    let (buckets, key) = (Arc::clone(&self.buckets), key.to_owned());
    Effect::from_ctx(move |ctx: Ctx<HNil>| {
      std::future::ready(buckets.take(&key, ctx.fiber().executor().now()))
    })
  }

  fn try_acquire_now(&self, key: &str) -> Option<Result<(), RateLimitError>> {
    Some(self.buckets.take(key, Instant::now()))
  }
}

type KeyFn = dyn Fn(&Request) -> Option<String> + Send + Sync;

/// Which bucket a request draws from; `None` exempts the request.
#[derive(Clone)]
pub struct RateLimitKey(Arc<KeyFn>);

impl fmt::Debug for RateLimitKey {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("RateLimitKey(..)")
  }
}

impl RateLimitKey {
  /// The key computed by `f`.
  pub fn from_fn(f: impl Fn(&Request) -> Option<String> + Send + Sync + 'static) -> Self {
    Self(Arc::new(f))
  }

  /// `ip:<peer ip>` from `ConnectInfo<SocketAddr>` (the [`ClientIp`] with no trusted proxy);
  /// `None` when the app is not served with connect info.
  pub fn peer_ip() -> Self {
    Self::client_ip(TrustedProxies::none())
  }

  /// `ip:<client ip>`: the [`ClientIp`] behind `trusted` proxies (the peer, or the rightmost
  /// `X-Forwarded-For` entry that is not a trusted proxy); `None` when the app is not served
  /// with connect info.
  pub fn client_ip(trusted: TrustedProxies) -> Self {
    Self::from_fn(move |req| {
      ClientIp::from_parts(req.extensions(), req.headers(), &trusted).map(|ip| format!("ip:{ip}"))
    })
  }

  /// `header:<name>:<value>` from request header `name` (e.g. an API key; for client addresses
  /// behind a proxy use [`Self::client_ip`]); `None` when the header is absent.
  pub fn header(name: HeaderName) -> Self {
    Self::from_fn(move |req| {
      let value = req.headers().get(&name)?;
      Some(format!(
        "header:{name}:{}",
        String::from_utf8_lossy(value.as_bytes())
      ))
    })
  }

  /// `tenant:<id>`: one bucket per tenant (organisation, customer, workspace) — a per-tenant
  /// limit, shared by every user and instance of that tenant when the store is.
  ///
  /// The tenant is read from a value of type `T` the request carries: the latest [`RequestCaps`]
  /// override of `T` (what an authentication layer or a
  /// [`CapOverrideLayer`](crate::CapOverrideLayer) recorded), else a request extension of type
  /// `T`. `tenant` maps it to the tenant id, so `T`
  /// can be the app's principal type (`|p: &CurrentPrincipal| p.tenant().map(str::to_owned)`)
  /// or a tenant type of its own. `None` — no such value, or a value without a tenant — leaves
  /// the request to a fallback (`.or(RateLimitKey::peer_ip())`), or exempts it.
  ///
  /// The layer must run after whatever records `T`: add the rate limit layer before (inside)
  /// the authentication layer.
  pub fn tenant<T: Send + Sync + 'static>(
    tenant: impl Fn(&T) -> Option<String> + Send + Sync + 'static,
  ) -> Self {
    Self::from_fn(move |req| {
      let id = match req
        .extensions()
        .get::<RequestCaps>()
        .and_then(RequestCaps::get::<T>)
      {
        Some(value) => tenant(&value),
        None => tenant(req.extensions().get::<T>()?),
      }?;
      Some(format!("tenant:{id}"))
    })
  }

  /// One bucket for every request.
  pub fn global() -> Self {
    Self::from_fn(|_| Some("global".to_owned()))
  }

  /// This key, or `fallback` when this one is `None`.
  #[must_use]
  pub fn or(self, fallback: RateLimitKey) -> Self {
    Self::from_fn(move |req| (self.0)(req).or_else(|| (fallback.0)(req)))
  }

  /// The key of `request`.
  pub fn key(&self, request: &Request) -> Option<String> {
    (self.0)(request)
  }
}

impl Default for RateLimitKey {
  /// [`Self::peer_ip`], else [`Self::global`].
  fn default() -> Self {
    Self::peer_ip().or(Self::global())
  }
}

/// Answers `429` when the request's bucket is empty (see the [module docs](self)).
#[derive(Clone)]
pub struct RateLimitLayer {
  store: Arc<dyn RateLimitStore>,
  key: RateLimitKey,
  retry_after: Duration,
  executor: EffectExecutor,
}

impl fmt::Debug for RateLimitLayer {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("RateLimitLayer")
      .field("key", &self.key)
      .field("retry_after", &self.retry_after)
      .finish_non_exhaustive()
  }
}

impl RateLimitLayer {
  /// Buckets in `store`, keyed by [`RateLimitKey::default`], `Retry-After: 1`.
  pub fn new(store: impl RateLimitStore) -> Self {
    Self {
      store: Arc::new(store),
      key: RateLimitKey::default(),
      retry_after: Duration::from_secs(1),
      // Its own unbounded executor: the check must not wait for an in-flight slot of the global
      // one.
      executor: EffectExecutor::new(),
    }
  }

  /// A [`MemoryRateLimitStore`] of `rate` requests per second with bursts of `burst`, per client
  /// IP; `Retry-After` is the time one token takes to refill.
  pub fn per_second(rate: f64, burst: f64) -> Self {
    let refill = if rate > 0.0 {
      Duration::try_from_secs_f64(1.0 / rate).unwrap_or(Duration::MAX)
    } else {
      Duration::from_secs(60)
    };
    Self::new(MemoryRateLimitStore::new(rate, burst)).retry_after(refill)
  }

  /// Draw from the bucket named by `key`.
  #[must_use]
  pub fn key(mut self, key: RateLimitKey) -> Self {
    self.key = key;
    self
  }

  /// `Retry-After` of a `429`, rounded up to whole seconds (at least 1).
  #[must_use]
  pub fn retry_after(mut self, after: Duration) -> Self {
    self.retry_after = after;
    self
  }

  fn retry_after_secs(&self) -> u64 {
    let secs = self.retry_after.as_secs();
    let secs = if self.retry_after.subsec_nanos() > 0 {
      secs.saturating_add(1)
    } else {
      secs
    };
    secs.max(1)
  }

  fn too_many(&self, path: &str) -> Response {
    let secs = self.retry_after_secs();
    let mut response = problem_response(
      StatusCode::TOO_MANY_REQUESTS,
      format!("rate limit exceeded; retry after {secs} s"),
      path,
    );
    response
      .headers_mut()
      .insert(header::RETRY_AFTER, HeaderValue::from(secs));
    response
  }

  async fn handle<S>(self: Arc<Self>, mut inner: S, request: Request) -> Response
  where
    S: Service<Request, Error = Infallible>,
    S::Response: IntoResponse,
  {
    if let Some(key) = self.key.key(&request) {
      let limited = match self.store.try_acquire_now(&key) {
        // An in-process store answers inline: no executor round trip.
        Some(outcome) => outcome.is_err(),
        None => matches!(
          self.executor.run_effect(self.store.acquire(&key)).await,
          Exit::Failure(Cause::Fail(RateLimitError))
        ),
      };
      if limited {
        return self.too_many(request.uri().path());
      }
    }
    match inner.call(request).await {
      Ok(response) => response.into_response(),
      Err(never) => match never {},
    }
  }
}

impl<S> Layer<S> for RateLimitLayer {
  type Service = RateLimitService<S>;

  fn layer(&self, inner: S) -> Self::Service {
    RateLimitService {
      inner,
      config: Arc::new(self.clone()),
    }
  }
}

/// The [`Service`] produced by [`RateLimitLayer`].
#[derive(Clone, Debug)]
pub struct RateLimitService<S> {
  inner: S,
  config: Arc<RateLimitLayer>,
}

impl<S> Service<Request> for RateLimitService<S>
where
  S: Service<Request, Error = Infallible> + Clone + Send + 'static,
  S::Response: IntoResponse,
  S::Future: Send + 'static,
{
  type Response = Response;
  type Error = Infallible;
  type Future = Pin<Box<dyn Future<Output = Result<Response, Infallible>> + Send>>;

  fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
    self.inner.poll_ready(cx)
  }

  fn call(&mut self, request: Request) -> Self::Future {
    // The ready service handles this request; the clone waits for the next `poll_ready`.
    let clone = self.inner.clone();
    let inner = std::mem::replace(&mut self.inner, clone);
    let config = Arc::clone(&self.config);
    Box::pin(async move { Ok(config.handle(inner, request).await) })
  }
}

#[cfg(test)]
mod tests {
  use std::net::SocketAddr;

  use super::*;
  use axum::body::Body;
  use axum::extract::ConnectInfo;

  fn request() -> Request {
    Request::builder().uri("/").body(Body::empty()).unwrap()
  }

  #[test]
  fn memory_store_evicts_the_least_recently_used_bucket() {
    let store = MemoryRateLimitStore::new(1.0, 1.0).shards(1).max_keys(2);
    assert!(store.is_empty());
    store.bucket("a");
    store.bucket("b");
    store.bucket("a");
    store.bucket("c");
    assert_eq!(store.len(), 2);
    assert_eq!(store.keys(), ["a", "c"]);
    assert!(format!("{store:?}").contains("max_keys: 2"));
  }

  #[test]
  fn m8_eviction_does_not_scan_all_keys() {
    let store = MemoryRateLimitStore::new(1.0, 1.0)
      .shards(1)
      .max_keys(10_000);
    for i in 0..10_000 {
      store.bucket(&format!("k{i}"));
    }
    assert_eq!(store.eviction_visits(), 0);
    // A full store: each new key evicts by looking at one slot, not at all 10,000.
    for i in 10_000..11_000 {
      store.bucket(&format!("k{i}"));
    }
    assert_eq!(store.len(), 10_000);
    assert_eq!(store.eviction_visits(), 1_000);
    // A hit moves its key to the front and evicts nothing.
    store.bucket("k10999");
    assert_eq!(store.eviction_visits(), 1_000);
  }

  #[test]
  fn inline_and_effect_checks_share_the_bucket() {
    let store = MemoryRateLimitStore::new(0.0, 2.0);
    assert_eq!(store.try_acquire_now("k"), Some(Ok(())));
    assert!(matches!(store.acquire("k").block_on(), Exit::Success(())));
    assert_eq!(store.try_acquire_now("k"), Some(Err(RateLimitError)));
    assert!(matches!(
      store.acquire("k").block_on(),
      Exit::Failure(Cause::Fail(RateLimitError))
    ));
    // Refill: 50 tokens per second, a burst of one.
    let fast = MemoryRateLimitStore::new(50.0, 1.0);
    assert_eq!(fast.try_acquire_now("k"), Some(Ok(())));
    assert_eq!(fast.try_acquire_now("k"), Some(Err(RateLimitError)));
    std::thread::sleep(Duration::from_millis(40));
    assert_eq!(fast.try_acquire_now("k"), Some(Ok(())));
    // A store without an inline check leaves the layer to run its effect.
    struct Remote;
    impl RateLimitStore for Remote {
      fn acquire(&self, _key: &str) -> Effect<(), RateLimitError, HNil> {
        id_effect::succeed(())
      }
    }
    assert_eq!(Arc::new(Remote).try_acquire_now("k"), None);
  }

  #[test]
  fn shards_split_max_keys_and_hold_it() {
    let store = MemoryRateLimitStore::new(1.0, 1.0).shards(8).max_keys(20);
    let caps: Vec<usize> = store
      .buckets
      .shards
      .iter()
      .map(|s| lock(s).capacity)
      .collect();
    assert_eq!(caps.iter().sum::<usize>(), 20);
    assert!(caps.iter().all(|&c| c == 2 || c == 3));
    for i in 0..1_000 {
      store.bucket(&format!("k{i}"));
    }
    assert!(store.len() <= 20);
    // Never more shards than keys.
    assert_eq!(
      MemoryRateLimitStore::new(1.0, 1.0)
        .max_keys(3)
        .shards(16)
        .buckets
        .shards
        .len(),
      3
    );
    assert!(MemoryRateLimitStore::default_shards() <= MemoryRateLimitStore::MAX_DEFAULT_SHARDS);
    assert!(MemoryRateLimitStore::default_shards().is_power_of_two());
  }

  #[test]
  fn keys_compose_and_fall_back() {
    let mut req = request();
    assert_eq!(RateLimitKey::peer_ip().key(&req), None);
    assert_eq!(RateLimitKey::default().key(&req).as_deref(), Some("global"));
    req
      .extensions_mut()
      .insert(ConnectInfo(SocketAddr::from(([10, 0, 0, 1], 4000))));
    assert_eq!(
      RateLimitKey::default().key(&req).as_deref(),
      Some("ip:10.0.0.1")
    );
    let api = RateLimitKey::header(HeaderName::from_static("x-api-key"));
    assert_eq!(api.key(&req), None);
    req
      .headers_mut()
      .insert("x-api-key", HeaderValue::from_static("k1"));
    assert_eq!(api.key(&req).as_deref(), Some("header:x-api-key:k1"));
    assert_eq!(format!("{api:?}"), "RateLimitKey(..)");
  }

  /// What an app's authentication records: a principal that may belong to a tenant.
  #[derive(Clone)]
  struct Member(Option<&'static str>);

  fn by_tenant() -> RateLimitKey {
    RateLimitKey::tenant(|m: &Member| m.0.map(str::to_owned))
  }

  #[test]
  fn tenant_keys_read_request_caps_then_extensions() {
    let mut req = request();
    assert_eq!(by_tenant().key(&req), None);
    // A plain extension.
    req.extensions_mut().insert(Member(Some("globex")));
    assert_eq!(by_tenant().key(&req).as_deref(), Some("tenant:globex"));
    // A `RequestCaps` override wins over the extension; the latest one counts.
    let mut caps = RequestCaps::new();
    caps
      .insert(Member(Some("initech")))
      .insert(Member(Some("acme")));
    req.extensions_mut().insert(caps);
    assert_eq!(by_tenant().key(&req).as_deref(), Some("tenant:acme"));
    // A principal without a tenant falls back.
    let mut caps = RequestCaps::new();
    caps.insert(Member(None));
    req.extensions_mut().insert(caps);
    assert_eq!(by_tenant().key(&req), None);
    assert_eq!(
      by_tenant().or(RateLimitKey::global()).key(&req).as_deref(),
      Some("global")
    );
  }

  #[tokio::test]
  async fn each_tenant_has_its_own_bucket() {
    use axum::Router;
    use axum::http::request::Parts;
    use axum::routing::get;
    use tower::ServiceExt;

    use crate::CapOverrideLayer;

    // The tenant a stand-in authentication layer records in `RequestCaps` from `x-tenant`.
    let authenticate = CapOverrideLayer::new(|parts: &Parts| {
      let tenant = parts.headers.get("x-tenant")?.to_str().ok()?;
      Some(Member(Some(if tenant == "acme" {
        "acme"
      } else {
        "globex"
      })))
    });
    let store = Arc::new(MemoryRateLimitStore::new(0.0, 2.0));
    let app = Router::new()
      .route("/api", get(|| async { "ok" }))
      .layer(RateLimitLayer::new(Arc::clone(&store)).key(by_tenant().or(RateLimitKey::global())))
      .layer(authenticate);
    let status = |tenant: Option<&str>| {
      let app = app.clone();
      let mut req = Request::builder().uri("/api");
      if let Some(tenant) = tenant {
        req = req.header("x-tenant", tenant);
      }
      let req = req.body(Body::empty()).unwrap();
      async move { app.oneshot(req).await.unwrap().status() }
    };

    // acme spends its burst of two; globex and the anonymous bucket are untouched.
    for _ in 0..2 {
      assert_eq!(status(Some("acme")).await, StatusCode::OK);
    }
    assert_eq!(status(Some("acme")).await, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(status(Some("globex")).await, StatusCode::OK);
    assert_eq!(status(None).await, StatusCode::OK);
    assert_eq!(store.keys(), ["global", "tenant:acme", "tenant:globex"]);
  }

  #[test]
  fn retry_after_rounds_up_to_whole_seconds() {
    let layer = RateLimitLayer::per_second(4.0, 1.0);
    assert_eq!(layer.retry_after_secs(), 1);
    assert_eq!(
      layer
        .clone()
        .retry_after(Duration::from_millis(2500))
        .retry_after_secs(),
      3
    );
    assert_eq!(
      layer
        .clone()
        .retry_after(Duration::from_secs(2))
        .retry_after_secs(),
      2
    );
    assert_eq!(RateLimitLayer::per_second(0.0, 1.0).retry_after_secs(), 60);
    assert!(format!("{layer:?}").contains("retry_after"));
  }
}
