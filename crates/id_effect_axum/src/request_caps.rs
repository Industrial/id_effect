//! Request-scoped capability overrides: a Tower/Axum layer that resolves per-request capability
//! values (tenant, database handle, request id, …) and installs them over the shared [`Env`] for
//! the effect run of that request only.
//!
//! This is the HTTP edge of [`id_effect::capability::with_override`]: the router state keeps the
//! base [`Env`]; [`CapOverrideLayer`] records overrides in the request's extensions as
//! [`RequestCaps`]; [`run_with_request_caps`] / [`execute_with_request_caps`] apply them to a clone
//! of the base env (an overlay) before building and running the effect. Other requests, and the
//! router state itself, never observe the override.
//!
//! ```rust
//! use axum::{Router, extract::State, routing::get};
//! use id_effect::{Cap, Effect, Env, succeed};
//! use id_effect_axum::{CapOverrideLayer, RequestCaps, execute_with_request_caps};
//!
//! #[derive(Clone)]
//! struct Tenant(String);
//!
//! fn whoami(env: &mut Env) -> Effect<String, std::convert::Infallible, Env> {
//!   succeed(env.get::<Cap<Tenant>>().0.clone())
//! }
//!
//! let mut base = Env::new();
//! base.insert::<Cap<Tenant>>(Tenant("public".into()));
//!
//! let app: Router = Router::new()
//!   .route(
//!     "/whoami",
//!     get(|st: State<Env>, caps: RequestCaps| execute_with_request_caps(st, caps, whoami)),
//!   )
//!   .layer(CapOverrideLayer::<Cap<Tenant>, _>::new(|parts| {
//!     let h = parts.headers.get("x-tenant")?.to_str().ok()?;
//!     Some(Tenant(h.to_owned()))
//!   }))
//!   .with_state(base);
//! # let _ = app;
//! ```

use std::convert::Infallible;
use std::fmt;
use std::marker::PhantomData;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::extract::{FromRequestParts, State};
use axum::http::Request;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use id_effect::Effect;
use id_effect::capability::{CapabilityKey, Env};
use tower_layer::Layer;
use tower_service::Service;

type Apply = Arc<dyn Fn(&mut Env) + Send + Sync>;

/// Capability overrides collected for one request (stored in request extensions).
///
/// Extract it in a handler (it is never rejected; absent means empty) and pass it to
/// [`run_with_request_caps`] / [`execute_with_request_caps`], or apply it yourself with
/// [`RequestCaps::overlay`]. Later inserts win over earlier ones for the same key.
#[derive(Clone, Default)]
pub struct RequestCaps {
  layers: Vec<Apply>,
}

impl RequestCaps {
  /// No overrides.
  #[inline]
  pub fn new() -> Self {
    Self::default()
  }

  /// Override capability `K` with `value` for this request.
  pub fn insert<K>(&mut self, value: K::Value) -> &mut Self
  where
    K: CapabilityKey,
  {
    self.layers.push(Arc::new(move |env: &mut Env| {
      env.insert::<K>(value.clone());
    }));
    self
  }

  /// Number of recorded overrides.
  #[inline]
  pub fn len(&self) -> usize {
    self.layers.len()
  }

  /// `true` when no override was recorded.
  #[inline]
  pub fn is_empty(&self) -> bool {
    self.layers.is_empty()
  }

  /// Apply every override to `env` in insertion order.
  pub fn apply_to(&self, env: &mut Env) {
    for apply in &self.layers {
      apply(env);
    }
  }

  /// Clone `base` and apply the overrides to the clone; `base` is left untouched.
  pub fn overlay(&self, base: &Env) -> Env {
    let mut env = base.clone();
    self.apply_to(&mut env);
    env
  }
}

impl fmt::Debug for RequestCaps {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("RequestCaps")
      .field("len", &self.len())
      .finish()
  }
}

impl<S: Send + Sync> FromRequestParts<S> for RequestCaps {
  type Rejection = Infallible;

  async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
    Ok(
      parts
        .extensions
        .get::<RequestCaps>()
        .cloned()
        .unwrap_or_default(),
    )
  }
}

/// Layer that resolves capability `K` from each request's [`Parts`] and records it in
/// [`RequestCaps`]. Returning `None` leaves the base capability in place.
///
/// Stack several layers (one per key) to override several capabilities.
pub struct CapOverrideLayer<K, F> {
  resolve: F,
  _key: PhantomData<fn() -> K>,
}

impl<K, F> CapOverrideLayer<K, F>
where
  K: CapabilityKey,
  F: Fn(&Parts) -> Option<K::Value> + Clone + Send + Sync + 'static,
{
  /// Resolve the override for capability `K` with `resolve`.
  #[inline]
  pub fn new(resolve: F) -> Self {
    Self {
      resolve,
      _key: PhantomData,
    }
  }
}

impl<K, F: Clone> Clone for CapOverrideLayer<K, F> {
  fn clone(&self) -> Self {
    Self {
      resolve: self.resolve.clone(),
      _key: PhantomData,
    }
  }
}

impl<K, F> fmt::Debug for CapOverrideLayer<K, F> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("CapOverrideLayer")
      .field("key", &std::any::type_name::<K>())
      .finish()
  }
}

impl<Svc, K, F: Clone> Layer<Svc> for CapOverrideLayer<K, F> {
  type Service = CapOverride<Svc, K, F>;

  fn layer(&self, inner: Svc) -> Self::Service {
    CapOverride {
      inner,
      resolve: self.resolve.clone(),
      _key: PhantomData,
    }
  }
}

/// Service produced by [`CapOverrideLayer`].
pub struct CapOverride<Svc, K, F> {
  inner: Svc,
  resolve: F,
  _key: PhantomData<fn() -> K>,
}

impl<Svc: Clone, K, F: Clone> Clone for CapOverride<Svc, K, F> {
  fn clone(&self) -> Self {
    Self {
      inner: self.inner.clone(),
      resolve: self.resolve.clone(),
      _key: PhantomData,
    }
  }
}

impl<Svc, K, F> fmt::Debug for CapOverride<Svc, K, F> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("CapOverride")
      .field("key", &std::any::type_name::<K>())
      .finish()
  }
}

impl<Svc, K, F, B> Service<Request<B>> for CapOverride<Svc, K, F>
where
  Svc: Service<Request<B>>,
  K: CapabilityKey,
  F: Fn(&Parts) -> Option<K::Value>,
{
  type Response = Svc::Response;
  type Error = Svc::Error;
  type Future = Svc::Future;

  #[inline]
  fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
    self.inner.poll_ready(cx)
  }

  fn call(&mut self, req: Request<B>) -> Self::Future {
    let (mut parts, body) = req.into_parts();
    if let Some(value) = (self.resolve)(&parts) {
      let mut caps = parts.extensions.remove::<RequestCaps>().unwrap_or_default();
      caps.insert::<K>(value);
      parts.extensions.insert(caps);
    }
    self.inner.call(Request::from_parts(parts, body))
  }
}

/// Like [`crate::run_with_caps`], with this request's [`RequestCaps`] overlaid on the base env.
pub async fn run_with_request_caps<A, E, F>(
  State(env): State<Env>,
  caps: RequestCaps,
  build: F,
) -> Result<A, E>
where
  A: Send + 'static,
  E: Send + 'static,
  F: FnOnce(&mut Env) -> Effect<A, E, Env> + Send + 'static,
{
  crate::run_with_env(caps.overlay(&env), build).await
}

/// Like [`crate::execute`] for an [`Env`] state, with this request's [`RequestCaps`] overlaid
/// (`503` when the executor is saturated under [`crate::SaturationPolicy::Reject`]).
pub async fn execute_with_request_caps<A, E, F>(
  State(env): State<Env>,
  caps: RequestCaps,
  build: F,
) -> Response
where
  A: IntoResponse + Send + 'static,
  E: IntoResponse + Send + 'static,
  F: FnOnce(&mut Env) -> Effect<A, E, Env> + Send + 'static,
{
  crate::respond(crate::try_run_effect_from_state(caps.overlay(&env), build).await)
}

#[cfg(test)]
mod tests {
  use super::*;
  use axum::Router;
  use axum::body::Body;
  use axum::http::StatusCode;
  use axum::routing::get;
  use id_effect::{Cap, succeed};
  use tower::ServiceExt;

  #[derive(Clone)]
  struct Tenant(String);

  #[derive(Clone, Copy)]
  struct Shard(u8);

  fn whoami(env: &mut Env) -> Effect<String, Infallible, Env> {
    let t = env.get::<Cap<Tenant>>().0.clone();
    let s = env.try_get::<Cap<Shard>>().map_or(0, |s| s.0);
    succeed(format!("{t}/{s}"))
  }

  fn app(base: Env) -> Router {
    Router::new()
      .route(
        "/",
        get(|st: State<Env>, caps: RequestCaps| execute_with_request_caps(st, caps, whoami)),
      )
      .layer(CapOverrideLayer::<Cap<Shard>, _>::new(|parts: &Parts| {
        parts
          .headers
          .get("x-shard")?
          .to_str()
          .ok()?
          .parse()
          .ok()
          .map(Shard)
      }))
      .layer(CapOverrideLayer::<Cap<Tenant>, _>::new(|parts: &Parts| {
        let h = parts.headers.get("x-tenant")?.to_str().ok()?;
        Some(Tenant(h.to_owned()))
      }))
      .with_state(base)
  }

  async fn body(app: Router, req: Request<Body>) -> String {
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = http_body_util::BodyExt::collect(res.into_body())
      .await
      .unwrap()
      .to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
  }

  // Current-thread on purpose: the executor strategy must not need a multi-thread runtime.
  #[tokio::test(flavor = "current_thread")]
  async fn override_applies_per_request_only() {
    let mut base = Env::new();
    base.insert::<Cap<Tenant>>(Tenant("public".into()));
    let app = app(base.clone());

    let req = Request::builder()
      .uri("/")
      .header("x-tenant", "acme")
      .header("x-shard", "7")
      .body(Body::empty())
      .unwrap();
    assert_eq!(body(app.clone(), req).await, "acme/7");

    let plain = Request::builder().uri("/").body(Body::empty()).unwrap();
    assert_eq!(body(app, plain).await, "public/0");
    assert_eq!(base.get::<Cap<Tenant>>().0, "public");
  }

  #[tokio::test(flavor = "current_thread")]
  async fn concurrent_requests_see_their_own_tenant() {
    let mut base = Env::new();
    base.insert::<Cap<Tenant>>(Tenant("public".into()));
    let app = app(base);
    let reqs = (0..16).map(|i| {
      let app = app.clone();
      async move {
        let req = Request::builder()
          .uri("/")
          .header("x-tenant", format!("t{i}"))
          .body(Body::empty())
          .unwrap();
        (i, body(app, req).await)
      }
    });
    for (i, got) in spawn_all(reqs).await {
      assert_eq!(got, format!("t{i}/0"));
    }
  }

  async fn spawn_all<F>(futs: impl Iterator<Item = F>) -> Vec<F::Output>
  where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
  {
    let handles: Vec<_> = futs.map(tokio::spawn).collect();
    let mut out = Vec::new();
    for h in handles {
      out.push(h.await.unwrap());
    }
    out
  }

  #[test]
  fn request_caps_overlay_leaves_base_untouched() {
    let mut base = Env::new();
    base.insert::<Cap<Tenant>>(Tenant("base".into()));
    let mut caps = RequestCaps::new();
    assert!(caps.is_empty());
    caps
      .insert::<Cap<Tenant>>(Tenant("a".into()))
      .insert::<Cap<Tenant>>(Tenant("b".into()));
    assert_eq!(caps.len(), 2);
    assert!(format!("{caps:?}").contains("len: 2"));
    let over = caps.overlay(&base);
    assert_eq!(over.get::<Cap<Tenant>>().0, "b");
    assert_eq!(base.get::<Cap<Tenant>>().0, "base");
    let layer = CapOverrideLayer::<Cap<Tenant>, _>::new(|_: &Parts| None);
    assert!(format!("{:?}", layer.clone()).contains("CapOverrideLayer"));
  }

  #[tokio::test(flavor = "current_thread")]
  async fn run_with_request_caps_without_layer_uses_base() {
    let mut base = Env::new();
    base.insert::<Cap<Tenant>>(Tenant("only".into()));
    let out = run_with_request_caps(State(base), RequestCaps::default(), whoami)
      .await
      .unwrap();
    assert_eq!(out, "only/0");
  }
}
