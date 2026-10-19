//! Deny-by-default access for typed routes (ADR 0013 R1, finding H8).
//!
//! Every typed [`Route`](super::Route) makes an access decision before a
//! [`Router`](super::Router) accepts it, and the type system keeps the score:
//!
//! - [`Route::public`](super::Route::public) — anyone may call it, anonymous callers included
//!   ([`Public`]);
//! - [`Route::guard`](super::Route::guard) — a [`RouteGuard`] decides before the handler runs
//!   ([`Guarded`]; repeatable, outermost last).
//!
//! A route with neither is [`Undecided`], and [`Router::route`](super::Router::route) does not
//! accept it: forgetting the decision is a compile error, not an open endpoint. The deprecated
//! [`Router::route_unchecked`](super::Router::route_unchecked) routes it anyway for one minor
//! version and records the opt-out in the
//! [posture registry](id_effect::observability::posture).
//!
//! At run time [`Router::into_axum`](super::Router::into_axum) installs the [`AccessLayer`]: a
//! request whose matched route is not a registered public or guarded typed route — a plain Axum
//! route merged with [`Router::merge_axum`](super::Router::merge_axum) — answers **404** Problem
//! Details unless the router took
//! [`allow_unregistered_routes_dangerous`](super::Router::allow_unregistered_routes_dangerous).
//! Routes merged onto the returned [`axum::Router`] are outside that layer; apply
//! [`Router::access_layer`](super::Router::access_layer) last to cover them too.
//!
//! [`RouteGuard`] is this crate's trait so that the router does not depend on an authorization
//! crate: `id_effect_authz`'s `Guard` implements it.
//!
//! ```rust
//! use axum::http::StatusCode;
//! use axum::http::request::Parts;
//! use axum::response::IntoResponse;
//! use id_effect::{Effect, succeed};
//! use id_effect_axum::route::{GuardFuture, Input, Route, RouteAccess, RouteGuard, Router};
//!
//! /// Admits requests carrying `x-staff: yes`.
//! struct Staff;
//!
//! impl RouteGuard for Staff {
//!   fn policy(&self) -> &str {
//!     "staff"
//!   }
//!
//!   fn check<'a>(&'a self, parts: &'a Parts) -> GuardFuture<'a> {
//!     let staff = parts.headers.get("x-staff").is_some_and(|v| v == "yes");
//!     Box::pin(async move {
//!       if staff { Ok(()) } else { Err(StatusCode::FORBIDDEN.into_response()) }
//!     })
//!   }
//! }
//!
//! fn hello(_: &mut (), _: Input) -> Effect<&'static str, std::convert::Infallible, ()> {
//!   succeed("hello")
//! }
//!
//! let router = Router::new()
//!   .route(Route::get("/hello").public().handler(hello))
//!   .route(Route::get("/staff").guard(Staff).handler(hello));
//! let report = router.access_report();
//! assert_eq!(report.public, ["GET /hello"]);
//! assert_eq!(router.registry().find(&axum::http::Method::GET, "/staff").unwrap().access, RouteAccess::Guarded);
//! ```
//!
//! An undecided route does not compile:
//!
//! ```compile_fail
//! use id_effect::{Effect, succeed};
//! use id_effect_axum::route::{Input, Route, Router};
//!
//! fn hello(_: &mut (), _: Input) -> Effect<&'static str, std::convert::Infallible, ()> {
//!   succeed("hello")
//! }
//!
//! let _ = Router::new().route(Route::get("/hello").handler(hello));
//! ```

use std::collections::HashMap;
use std::convert::Infallible;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::extract::MatchedPath;
use axum::http::request::Parts;
use axum::http::{Method, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use tower_layer::Layer;
use tower_service::Service;

use super::problem::Problem;
use super::registry::{RouteAccess, RouteInfo};

mod sealed {
  pub trait Sealed {}
}

/// The access state of a [`Route`](super::Route) or [`Endpoint`](super::Endpoint):
/// [`Undecided`], [`Public`] or [`Guarded`]. Sealed.
pub trait Access: sealed::Sealed + Send + Sync + 'static {
  /// The [`RouteAccess`] recorded in the route's [`RouteInfo`].
  const ACCESS: RouteAccess;
}

/// An access state [`Router::route`](super::Router::route) accepts: [`Public`] or [`Guarded`].
/// Sealed.
#[diagnostic::on_unimplemented(
  message = "the route has no access decision",
  label = "this endpoint is neither public nor guarded",
  note = "call `.public()` or `.guard(g)` on the route (or `.authorize(guard)` on the endpoint) before `Router::route`"
)]
pub trait Decided: Access {}

/// No access decision yet: [`Router::route`](super::Router::route) rejects the endpoint.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Undecided;

/// Anyone may call the route, anonymous callers included
/// ([`Route::public`](super::Route::public)).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Public;

/// One or more [`RouteGuard`]s decide before the handler runs
/// ([`Route::guard`](super::Route::guard)).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Guarded;

impl sealed::Sealed for Undecided {}
impl sealed::Sealed for Public {}
impl sealed::Sealed for Guarded {}

impl Access for Undecided {
  const ACCESS: RouteAccess = RouteAccess::Undecided;
}

impl Access for Public {
  const ACCESS: RouteAccess = RouteAccess::Public;
}

impl Access for Guarded {
  const ACCESS: RouteAccess = RouteAccess::Guarded;
}

impl Decided for Public {}
impl Decided for Guarded {}

/// The future of [`RouteGuard::check`]: `Ok(())` lets the request through, `Err(response)`
/// answers it without running the handler.
pub type GuardFuture<'a> = Pin<Box<dyn Future<Output = Result<(), Response>> + Send + 'a>>;

/// A per-route access check, evaluated on the request head before the handler.
///
/// Implemented by `id_effect_authz`'s `Guard`; implement it for any other check (an API key, a
/// network allowlist). [`policy`](RouteGuard::policy) is recorded in
/// [`RouteInfo::policies`] and documented by OpenAPI.
pub trait RouteGuard: Send + Sync + 'static {
  /// The policy name this guard enforces.
  fn policy(&self) -> &str;

  /// Decide for the request head `parts`: `Ok(())` to continue, `Err(response)` to answer with
  /// it (typically 401 or 403 Problem Details).
  fn check<'a>(&'a self, parts: &'a Parts) -> GuardFuture<'a>;
}

/// The guards of a route, in the order they were added (outermost last).
#[derive(Clone, Default)]
pub(crate) struct Guards(pub(crate) Vec<Arc<dyn RouteGuard>>);

impl Guards {
  pub(crate) fn policies(&self) -> impl Iterator<Item = String> + '_ {
    self.0.iter().map(|guard| guard.policy().to_owned())
  }
}

impl fmt::Debug for Guards {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_list()
      .entries(self.0.iter().map(|guard| guard.policy()))
      .finish()
  }
}

/// Wraps one route's handler in a [`RouteGuard`].
#[derive(Clone)]
pub(crate) struct GuardLayer(pub(crate) Arc<dyn RouteGuard>);

impl<S> Layer<S> for GuardLayer {
  type Service = GuardService<S>;

  fn layer(&self, inner: S) -> Self::Service {
    GuardService {
      guard: Arc::clone(&self.0),
      inner,
    }
  }
}

/// The service a [`GuardLayer`] wraps a handler in.
#[derive(Clone)]
pub(crate) struct GuardService<S> {
  guard: Arc<dyn RouteGuard>,
  inner: S,
}

impl<S, B> Service<Request<B>> for GuardService<S>
where
  S: Service<Request<B>, Response = Response, Error = Infallible> + Clone + Send + 'static,
  S::Future: Send + 'static,
  B: Send + 'static,
{
  type Response = Response;
  type Error = Infallible;
  type Future = Pin<Box<dyn Future<Output = Result<Response, Infallible>> + Send>>;

  fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
    self.inner.poll_ready(cx)
  }

  fn call(&mut self, req: Request<B>) -> Self::Future {
    let ready = self.inner.clone();
    let mut inner = std::mem::replace(&mut self.inner, ready);
    let guard = Arc::clone(&self.guard);
    Box::pin(async move {
      let (parts, body) = req.into_parts();
      match guard.check(&parts).await {
        Ok(()) => inner.call(Request::from_parts(parts, body)).await,
        Err(rejection) => Ok(rejection),
      }
    })
  }
}

/// The deny-by-default layer: a request whose matched route is not a registered public, guarded
/// (or [unchecked](super::Router::route_unchecked)) typed route answers **404** Problem Details
/// without reaching the handler.
///
/// [`Router::into_axum`](super::Router::into_axum) installs it on the router's own routes. To
/// cover plain routes merged onto the returned [`axum::Router`] as well, add
/// [`Router::access_layer`](super::Router::access_layer) last, with `route_layer`:
///
/// ```rust
/// use id_effect::{Effect, succeed};
/// use id_effect_axum::route::{Input, Route, Router};
///
/// fn hello(_: &mut (), _: Input) -> Effect<&'static str, std::convert::Infallible, ()> {
///   succeed("hello")
/// }
///
/// let router = Router::new().route(Route::get("/hello").public().handler(hello));
/// let access = router.access_layer();
/// let app: axum::Router = router
///   .into_axum()
///   .route("/debug", axum::routing::get(|| async { "internals" })) // answers 404
///   .route_layer(access);
/// # let _ = app;
/// ```
#[derive(Clone, Debug)]
pub struct AccessLayer {
  served: Arc<HashMap<String, Vec<Method>>>,
}

impl AccessLayer {
  /// The layer admitting the served routes of `registry` (every access but
  /// [`RouteAccess::Undecided`]).
  pub fn new<'a>(registry: impl IntoIterator<Item = &'a RouteInfo>) -> Self {
    let mut served: HashMap<String, Vec<Method>> = HashMap::new();
    for info in registry {
      if info.access.is_served() {
        served
          .entry(info.path.clone())
          .or_default()
          .push(info.method.clone());
      }
    }
    Self {
      served: Arc::new(served),
    }
  }

  /// Whether `method` on the route template `path` is served (`HEAD` follows `GET`).
  pub fn admits(&self, method: &Method, path: &str) -> bool {
    self.served.get(path).is_some_and(|methods| {
      methods.contains(method) || (*method == Method::HEAD && methods.contains(&Method::GET))
    })
  }
}

impl<S> Layer<S> for AccessLayer {
  type Service = AccessService<S>;

  fn layer(&self, inner: S) -> Self::Service {
    AccessService {
      layer: self.clone(),
      inner,
    }
  }
}

/// The service an [`AccessLayer`] wraps a route in.
#[derive(Clone, Debug)]
pub struct AccessService<S> {
  layer: AccessLayer,
  inner: S,
}

impl<S, B> Service<Request<B>> for AccessService<S>
where
  S: Service<Request<B>, Response = Response, Error = Infallible> + Send + 'static,
  S::Future: Send + 'static,
  B: Send + 'static,
{
  type Response = Response;
  type Error = Infallible;
  type Future = Pin<Box<dyn Future<Output = Result<Response, Infallible>> + Send>>;

  fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
    self.inner.poll_ready(cx)
  }

  fn call(&mut self, req: Request<B>) -> Self::Future {
    let admitted = req
      .extensions()
      .get::<MatchedPath>()
      .is_some_and(|matched| self.layer.admits(req.method(), matched.as_str()));
    if admitted {
      Box::pin(self.inner.call(req))
    } else {
      let problem = Problem::new(StatusCode::NOT_FOUND)
        .with_detail("no route declares access to this resource")
        .with_instance(req.uri().path());
      Box::pin(std::future::ready(Ok(problem.into_response())))
    }
  }
}

/// Who may call each route of a [`Router`](super::Router)
/// ([`Router::access_report`](super::Router::access_report)); logged when the router becomes an
/// Axum router. Each route reads `METHOD /path`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct AccessReport {
  /// Routes anyone may call, anonymous callers included.
  pub public: Vec<String>,
  /// Guarded routes with their policy names (outermost last).
  pub guarded: Vec<(String, Vec<String>)>,
  /// Routes added with the deprecated [`Router::route_unchecked`](super::Router::route_unchecked).
  pub unchecked: Vec<String>,
  /// Whether plain routes without an access decision are served
  /// ([`Router::allow_unregistered_routes_dangerous`](super::Router::allow_unregistered_routes_dangerous)).
  pub unregistered_allowed: bool,
}

impl AccessReport {
  /// The report for `registry`.
  pub fn new<'a>(
    registry: impl IntoIterator<Item = &'a RouteInfo>,
    unregistered_allowed: bool,
  ) -> Self {
    let mut report = Self {
      unregistered_allowed,
      ..Self::default()
    };
    for info in registry {
      let route = format!("{} {}", info.method, info.path);
      match info.access {
        RouteAccess::Public => report.public.push(route),
        RouteAccess::Guarded => report.guarded.push((route, info.policies.clone())),
        RouteAccess::Unchecked => report.unchecked.push(route),
        RouteAccess::Undecided => {}
      }
    }
    report
  }

  /// `true` when the router serves something without a decision: an unchecked route, or
  /// unregistered routes.
  pub fn has_opt_outs(&self) -> bool {
    !self.unchecked.is_empty() || self.unregistered_allowed
  }

  pub(crate) fn log(&self) {
    tracing::info!(
      target: "id_effect_axum::route::access",
      public = ?self.public,
      guarded = self.guarded.len(),
      "typed routes: {} public, {} guarded",
      self.public.len(),
      self.guarded.len(),
    );
    if self.has_opt_outs() {
      tracing::warn!(
        target: "id_effect_axum::route::access",
        unchecked = ?self.unchecked,
        unregistered_allowed = self.unregistered_allowed,
        "routes served without an access decision",
      );
    }
  }
}

impl fmt::Display for AccessReport {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    for route in &self.public {
      writeln!(f, "public     {route}")?;
    }
    for (route, policies) in &self.guarded {
      writeln!(f, "guarded    {route}  [{}]", policies.join(", "))?;
    }
    for route in &self.unchecked {
      writeln!(f, "unchecked  {route}")?;
    }
    if self.unregistered_allowed {
      writeln!(f, "unregistered routes allowed")?;
    }
    Ok(())
  }
}
