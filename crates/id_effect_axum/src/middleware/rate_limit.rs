//! HTTP rate limiting: [`RateLimitLayer`] takes one token per request from the bucket of the
//! request's [`RateLimitKey`] and answers `429 Too Many Requests` (Problem, `Retry-After`) when it
//! is empty.
//!
//! Buckets live in a [`RateLimitStore`]. The admission check is an effect run on the effect
//! runtime, so a store can do I/O: [`MemoryRateLimitStore`] keeps one
//! [`id_effect_resilience::RateLimiter`] (token bucket) per key in process; a shared store (e.g.
//! Redis, for several instances) implements the same trait. A store that dies or is interrupted
//! lets the request through (fail open): the limiter protects capacity, it is not an access
//! control.
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
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};
use std::time::Duration;

use axum::extract::Request;
use axum::http::{HeaderName, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use id_effect::{Cause, Effect, Exit, HNil};
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
}

impl<T: RateLimitStore + ?Sized> RateLimitStore for Arc<T> {
  fn acquire(&self, key: &str) -> Effect<(), RateLimitError, HNil> {
    (**self).acquire(key)
  }
}

/// In-process store: one [`RateLimiter`] per key (`rate` tokens/second, burst `burst`), at most
/// [`Self::max_keys`] keys (the least recently used bucket is dropped first).
pub struct MemoryRateLimitStore {
  rate: f64,
  burst: f64,
  max_keys: usize,
  clock: AtomicU64,
  buckets: Mutex<HashMap<String, (RateLimiter, u64)>>,
}

impl fmt::Debug for MemoryRateLimitStore {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("MemoryRateLimitStore")
      .field("rate", &self.rate)
      .field("burst", &self.burst)
      .field("max_keys", &self.max_keys)
      .field("keys", &self.len())
      .finish()
  }
}

impl MemoryRateLimitStore {
  /// Default [`Self::max_keys`].
  pub const DEFAULT_MAX_KEYS: usize = 100_000;

  /// Buckets refilling `rate` tokens per second up to `burst` tokens (each starts full).
  pub fn new(rate: f64, burst: f64) -> Self {
    Self {
      rate,
      burst,
      max_keys: Self::DEFAULT_MAX_KEYS,
      clock: AtomicU64::new(0),
      buckets: Mutex::new(HashMap::new()),
    }
  }

  /// Keep at most `n` (at least 1) buckets.
  #[must_use]
  pub fn max_keys(mut self, n: usize) -> Self {
    self.max_keys = n.max(1);
    self
  }

  /// Buckets currently held.
  pub fn len(&self) -> usize {
    self.lock().len()
  }

  /// Whether no bucket is held.
  pub fn is_empty(&self) -> bool {
    self.len() == 0
  }

  fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, (RateLimiter, u64)>> {
    self.buckets.lock().unwrap_or_else(PoisonError::into_inner)
  }

  /// The bucket of `key`, created (evicting the least recently used one when full) if missing.
  fn bucket(&self, key: &str) -> RateLimiter {
    let now = self.clock.fetch_add(1, Ordering::Relaxed);
    let mut buckets = self.lock();
    if let Some((limiter, used)) = buckets.get_mut(key) {
      *used = now;
      return limiter.clone();
    }
    if buckets.len() >= self.max_keys
      && let Some(oldest) = buckets
        .iter()
        .min_by_key(|(_, (_, used))| *used)
        .map(|(k, _)| k.clone())
    {
      buckets.remove(&oldest);
    }
    let limiter = RateLimiter::new(self.rate, self.burst);
    buckets.insert(key.to_owned(), (limiter.clone(), now));
    limiter
  }
}

impl RateLimitStore for MemoryRateLimitStore {
  fn acquire(&self, key: &str) -> Effect<(), RateLimitError, HNil> {
    self.bucket(key).acquire()
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
      let exit = self.executor.run_effect(self.store.acquire(&key)).await;
      if let Exit::Failure(Cause::Fail(RateLimitError)) = exit {
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
    let store = MemoryRateLimitStore::new(1.0, 1.0).max_keys(2);
    assert!(store.is_empty());
    store.bucket("a");
    store.bucket("b");
    store.bucket("a");
    store.bucket("c");
    assert_eq!(store.len(), 2);
    let keys = store.lock();
    assert!(keys.contains_key("a") && keys.contains_key("c"));
    drop(keys);
    assert!(format!("{store:?}").contains("max_keys: 2"));
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
    let mut keys: Vec<String> = store.lock().keys().cloned().collect();
    keys.sort();
    assert_eq!(keys, ["global", "tenant:acme", "tenant:globex"]);
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
