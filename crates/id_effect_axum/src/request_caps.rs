//! Request-scoped capability overrides: a Tower/Axum layer that resolves per-request capability
//! values (tenant, database handle, request id, …) and installs them over the shared [`DynEnv`]
//! for the effect run of that request only.
//!
//! The router state keeps the base [`DynEnv`]; [`CapOverrideLayer`] records overrides in the
//! request's extensions as [`RequestCaps`]; [`run_with_request_caps`] / [`execute_with_request_caps`]
//! apply them to a clone of the base env (an overlay, one `Arc` bump per capability) before
//! building and running the effect. Other requests, and the router state itself, never observe
//! the override.
//!
//! Typed environments (`HCons<..>`) read an override with [`RequestCaps::get`] and `provide` it.
//!
//! ```rust
//! use axum::{Router, extract::State, routing::get};
//! use id_effect::{DynEnv, Effect, succeed};
//! use id_effect_axum::{CapOverrideLayer, RequestCaps, execute_with_request_caps};
//!
//! struct Tenant(String);
//!
//! fn whoami(env: &mut DynEnv) -> Effect<String, std::convert::Infallible, DynEnv> {
//!   let tenant = env.try_get::<Tenant>().map_or_else(|_| "none".into(), |t| t.0.clone());
//!   succeed(tenant)
//! }
//!
//! let base = DynEnv::new().with(Tenant("public".into()));
//!
//! let app: Router = Router::new()
//!   .route(
//!     "/whoami",
//!     get(|st: State<DynEnv>, caps: RequestCaps| execute_with_request_caps(st, caps, whoami)),
//!   )
//!   .layer(CapOverrideLayer::new(|parts| {
//!     let h = parts.headers.get("x-tenant")?.to_str().ok()?;
//!     Some(Tenant(h.to_owned()))
//!   }))
//!   .with_state(base);
//! # let _ = app;
//! ```

use std::any::{Any, TypeId};
use std::convert::Infallible;
use std::fmt;
use std::marker::PhantomData;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::extract::{FromRequestParts, State};
use axum::http::Request;
use axum::http::request::Parts;
use axum::response::Response;
use id_effect::Exit;
use id_effect::{DynEnv, Effect};
use tower_layer::Layer;
use tower_service::Service;

type Slot = (
  TypeId,
  Arc<dyn Any + Send + Sync>,
  fn(&mut DynEnv, Arc<dyn Any + Send + Sync>),
);

/// Capability overrides collected for one request (stored in request extensions).
///
/// Extract it in a handler (it is never rejected; absent means empty) and pass it to
/// [`run_with_request_caps`] / [`execute_with_request_caps`], or apply it yourself with
/// [`RequestCaps::overlay`]. Later inserts win over earlier ones for the same type.
#[derive(Clone, Default)]
pub struct RequestCaps {
  slots: Vec<Slot>,
}

fn install<T: Send + Sync + 'static>(env: &mut DynEnv, value: Arc<dyn Any + Send + Sync>) {
  if let Ok(v) = value.downcast::<T>() {
    env.insert_arc(v);
  }
}

impl RequestCaps {
  /// No overrides.
  #[inline]
  pub fn new() -> Self {
    Self::default()
  }

  /// Override capability `T` with `value` for this request.
  pub fn insert<T: Send + Sync + 'static>(&mut self, value: T) -> &mut Self {
    self.insert_arc(Arc::new(value))
  }

  /// Override capability `T` with an already shared value.
  pub fn insert_arc<T: Send + Sync + 'static>(&mut self, value: Arc<T>) -> &mut Self {
    self.slots.push((TypeId::of::<T>(), value, install::<T>));
    self
  }

  /// The latest override of `T`, if any.
  pub fn get<T: Send + Sync + 'static>(&self) -> Option<Arc<T>> {
    self
      .slots
      .iter()
      .rev()
      .find(|(id, _, _)| *id == TypeId::of::<T>())
      .and_then(|(_, v, _)| Arc::clone(v).downcast::<T>().ok())
  }

  /// Number of recorded overrides.
  #[inline]
  pub fn len(&self) -> usize {
    self.slots.len()
  }

  /// `true` when no override was recorded.
  #[inline]
  pub fn is_empty(&self) -> bool {
    self.slots.is_empty()
  }

  /// Apply every override to `env` in insertion order.
  pub fn apply_to(&self, env: &mut DynEnv) {
    for (_, value, install) in &self.slots {
      install(env, Arc::clone(value));
    }
  }

  /// Clone `base` and apply the overrides to the clone; `base` is left untouched.
  pub fn overlay(&self, base: &DynEnv) -> DynEnv {
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

/// Layer that resolves capability `T` from each request's [`Parts`] and records it in
/// [`RequestCaps`]. Returning `None` leaves the base capability in place.
///
/// Stack several layers (one per type) to override several capabilities.
pub struct CapOverrideLayer<T, F> {
  resolve: F,
  _cap: PhantomData<fn() -> T>,
}

impl<T, F> CapOverrideLayer<T, F>
where
  T: Send + Sync + 'static,
  F: Fn(&Parts) -> Option<T> + Clone + Send + Sync + 'static,
{
  /// Resolve the override for capability `T` with `resolve`.
  #[inline]
  pub fn new(resolve: F) -> Self {
    Self {
      resolve,
      _cap: PhantomData,
    }
  }
}

impl<T, F: Clone> Clone for CapOverrideLayer<T, F> {
  fn clone(&self) -> Self {
    Self {
      resolve: self.resolve.clone(),
      _cap: PhantomData,
    }
  }
}

impl<T, F> fmt::Debug for CapOverrideLayer<T, F> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("CapOverrideLayer")
      .field("capability", &std::any::type_name::<T>())
      .finish()
  }
}

impl<Svc, T, F: Clone> Layer<Svc> for CapOverrideLayer<T, F> {
  type Service = CapOverride<Svc, T, F>;

  fn layer(&self, inner: Svc) -> Self::Service {
    CapOverride {
      inner,
      resolve: self.resolve.clone(),
      _cap: PhantomData,
    }
  }
}

/// Service produced by [`CapOverrideLayer`].
pub struct CapOverride<Svc, T, F> {
  inner: Svc,
  resolve: F,
  _cap: PhantomData<fn() -> T>,
}

impl<Svc: Clone, T, F: Clone> Clone for CapOverride<Svc, T, F> {
  fn clone(&self) -> Self {
    Self {
      inner: self.inner.clone(),
      resolve: self.resolve.clone(),
      _cap: PhantomData,
    }
  }
}

impl<Svc, T, F> fmt::Debug for CapOverride<Svc, T, F> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("CapOverride")
      .field("capability", &std::any::type_name::<T>())
      .finish()
  }
}

impl<Svc, T, F, B> Service<Request<B>> for CapOverride<Svc, T, F>
where
  Svc: Service<Request<B>>,
  T: Send + Sync + 'static,
  F: Fn(&Parts) -> Option<T>,
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
      caps.insert(value);
      parts.extensions.insert(caps);
    }
    self.inner.call(Request::from_parts(parts, body))
  }
}

/// Like [`crate::run_with_caps`], with this request's [`RequestCaps`] overlaid on the base env.
pub async fn run_with_request_caps<A, E, F>(
  State(env): State<DynEnv>,
  caps: RequestCaps,
  build: F,
) -> Exit<A, E>
where
  A: Send + 'static,
  E: Send + 'static,
  F: FnOnce(&mut DynEnv) -> Effect<A, E, DynEnv> + Send + 'static,
{
  crate::run_with_env(caps.overlay(&env), build).await
}

/// Like [`crate::execute`] for a [`DynEnv`] state, with this request's [`RequestCaps`] overlaid
/// (`503` when the executor is saturated under [`crate::SaturationPolicy::Reject`]).
pub async fn execute_with_request_caps<A, E, F>(
  State(env): State<DynEnv>,
  caps: RequestCaps,
  build: F,
) -> Response
where
  A: axum::response::IntoResponse + Send + 'static,
  E: axum::response::IntoResponse + Send + 'static,
  F: FnOnce(&mut DynEnv) -> Effect<A, E, DynEnv> + Send + 'static,
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
  use id_effect::succeed;
  use tower::ServiceExt;

  struct Tenant(String);

  #[derive(Clone, Copy)]
  struct Shard(u8);

  fn whoami(env: &mut DynEnv) -> Effect<String, Infallible, DynEnv> {
    let t = env
      .try_get::<Tenant>()
      .map_or_else(|_| "?".into(), |t| t.0.clone());
    let s = env.try_get::<Shard>().map_or(0, |s| s.0);
    succeed(format!("{t}/{s}"))
  }

  fn app(base: DynEnv) -> Router {
    Router::new()
      .route(
        "/",
        get(|st: State<DynEnv>, caps: RequestCaps| execute_with_request_caps(st, caps, whoami)),
      )
      .layer(CapOverrideLayer::new(|parts: &Parts| {
        parts
          .headers
          .get("x-shard")?
          .to_str()
          .ok()?
          .parse()
          .ok()
          .map(Shard)
      }))
      .layer(CapOverrideLayer::new(|parts: &Parts| {
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

  // Current-thread on purpose: request fibers must not need a multi-thread runtime.
  #[tokio::test(flavor = "current_thread")]
  async fn override_applies_per_request_only() {
    let base = DynEnv::new().with(Tenant("public".into()));
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
    assert_eq!(base.try_get::<Tenant>().unwrap().0, "public");
  }

  #[tokio::test(flavor = "current_thread")]
  async fn concurrent_requests_see_their_own_tenant() {
    let app = app(DynEnv::new().with(Tenant("public".into())));
    let mut handles = Vec::new();
    for i in 0..16 {
      let app = app.clone();
      handles.push(tokio::spawn(async move {
        let req = Request::builder()
          .uri("/")
          .header("x-tenant", format!("t{i}"))
          .body(Body::empty())
          .unwrap();
        (i, body(app, req).await)
      }));
    }
    for h in handles {
      let (i, got) = h.await.unwrap();
      assert_eq!(got, format!("t{i}/0"));
    }
  }

  #[tokio::test]
  async fn run_with_request_caps_and_accessors() {
    let mut caps = RequestCaps::new();
    assert!(caps.is_empty());
    caps.insert(Shard(1)).insert(Shard(2));
    assert_eq!(caps.len(), 2);
    assert_eq!(caps.get::<Shard>().map(|s| s.0), Some(2));
    assert!(caps.get::<Tenant>().is_none());
    assert!(format!("{caps:?}").contains("len: 2"));
    let layer = CapOverrideLayer::new(|_: &Parts| Some(Shard(3)));
    assert!(format!("{:?}", layer.clone()).contains("Shard"));
    let out = run_with_request_caps(State(DynEnv::new()), caps, |env: &mut DynEnv| {
      succeed::<_, Infallible, _>(env.try_get::<Shard>().map_or(0, |s| s.0))
    })
    .await;
    assert_eq!(out, Exit::Success(2));
  }
}
