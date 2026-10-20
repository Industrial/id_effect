//! Route guards (feature `axum`): a [`Guard`] is a Tower layer that evaluates a policy before the
//! handler runs, and [`AuthorizeExt::authorize`] attaches one to a route.
//!
//! # Where the principal comes from
//!
//! The guard reads the request's principal from its extensions, in this order:
//!
//! 1. a [`CurrentPrincipal`] request extension — what an authentication layer inserts
//!    (`req.extensions_mut().insert(CurrentPrincipal::new(..))`; `id_effect_auth`'s middleware
//!    does this);
//! 2. a [`CurrentPrincipal`] recorded in [`RequestCaps`] by an
//!    [`id_effect_axum::CapOverrideLayer`] (the per-request capability override);
//! 3. otherwise the [`Anonymous`](crate::Anonymous) principal.
//!
//! The authentication layer must therefore run **before** (outside) the guard: add it with
//! `Router::layer` and the guard per route.
//!
//! # Typed routes: anonymous callers reach public routes only
//!
//! [`Guard`] is an [`id_effect_axum::route::RouteGuard`], so a typed route takes it with
//! `Route::guard(guard)` (or `.authorize(guard)` on the endpoint), and the router accepts no
//! route without a decision (ADR 0013 R1). On a typed route the guard answers **401** to the
//! anonymous principal before its policy runs, even a policy such as
//! [`allow_all`](crate::allow_all): a route anonymous callers may use is declared
//! `Route::public()`. As a plain Tower layer (`MethodRouter::authorize`, `.layer(guard)`) the
//! guard evaluates its policy for every principal, as before.
//!
//! # Answers
//!
//! Allowed requests reach the handler unchanged. A denial answers per
//! [`AuthzError::http_status`]: **401** for an anonymous principal, **403** otherwise, **500**
//! when the policy cannot be evaluated. The body is Problem Details (RFC 9457,
//! `application/problem+json`) with `type`, `title`, `status`, `detail` and the guard's `policy`
//! name; the denial reason is only included with [`Guard::expose_reason`]. The [`AuthzError`] is
//! also set as a response extension for logging middleware.
//!
//! ```
//! use axum::routing::get;
//! use id_effect_authz::guard::{AuthorizeExt, Guard};
//! use id_effect_authz::require_role;
//!
//! let app: axum::Router = axum::Router::new().route(
//!   "/ledger",
//!   get(|| async { "entries" }).authorize(Guard::new("ledger.read", require_role("viewer"))),
//! );
//! # let _ = app;
//! ```

use std::convert::Infallible;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::http::request::Parts;
use axum::http::{HeaderValue, Request, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::MethodRouter;
use id_effect::{Cause, Exit, fail};
use id_effect_axum::route::{Endpoint, GuardFuture, Guarded, RouteGuard, RouteInfo, RouteRegistry};
use id_effect_axum::{EffectExecutor, RequestCaps};
use serde_json::json;
use tower_layer::Layer;
use tower_service::Service;

use crate::audit::{AuthzEvents, audited};
use crate::policy::{AuthzError, Decision, Denial, Policy, PolicyEffect};
use crate::principal::CurrentPrincipal;
use crate::registry::Policies;

type Check = dyn Fn(&CurrentPrincipal, &Parts) -> PolicyEffect + Send + Sync;

/// The policy's [`Policy::decide_now`]: its decision without the effect runtime, if it has one.
type DecideNow = dyn Fn(&CurrentPrincipal, &Parts) -> Option<Decision> + Send + Sync;

/// A route guard: a named policy evaluated for the request's principal before the handler.
///
/// It is a Tower [`Layer`]; attach it with [`AuthorizeExt::authorize`] or any `.layer(..)` /
/// `.route_layer(..)`.
#[derive(Clone)]
pub struct Guard {
  name: Arc<str>,
  check: Arc<Check>,
  now: Arc<DecideNow>,
  events: Option<AuthzEvents>,
  expose_reason: bool,
}

impl Guard {
  /// A guard named `name` evaluating `policy` (a rule over the principal only).
  pub fn new(name: impl Into<String>, policy: impl Policy<()>) -> Self {
    let policy = Arc::new(policy);
    let pure = Arc::clone(&policy);
    Self::from_check(name, move |principal, _| policy.evaluate(principal, &()))
      .decide_now(move |principal, _| pure.decide_now(principal, &()))
  }

  /// A guard named `name` evaluating `policy` against the request head (method, URI, headers,
  /// extensions) — e.g. a tenant taken from the path.
  pub fn request(name: impl Into<String>, policy: impl Policy<Parts>) -> Self {
    let policy = Arc::new(policy);
    let pure = Arc::clone(&policy);
    Self::from_check(name, move |principal, parts| {
      policy.evaluate(principal, parts)
    })
    .decide_now(move |principal, parts| pure.decide_now(principal, parts))
  }

  /// A guard evaluating the policy `name` of `policies` (defined over `()`). An unknown name or
  /// a policy defined for another resource type answers 500 when a request arrives.
  pub fn named(policies: &Policies, name: impl Into<String>) -> Self {
    let name = name.into();
    let policies = policies.clone();
    let lookup = name.clone();
    Self::from_check(name, move |principal, _| {
      policies.evaluate(&lookup, principal, &())
    })
  }

  fn from_check(
    name: impl Into<String>,
    check: impl Fn(&CurrentPrincipal, &Parts) -> PolicyEffect + Send + Sync + 'static,
  ) -> Self {
    Self {
      name: Arc::from(name.into()),
      check: Arc::new(check),
      now: Arc::new(|_, _| None),
      events: None,
      expose_reason: false,
    }
  }

  fn decide_now(
    mut self,
    now: impl Fn(&CurrentPrincipal, &Parts) -> Option<Decision> + Send + Sync + 'static,
  ) -> Self {
    self.now = Arc::new(now);
    self
  }

  /// Records every decision of this guard to `events` (the resource is `METHOD /path`).
  #[must_use]
  pub fn events(mut self, events: AuthzEvents) -> Self {
    self.events = Some(events);
    self
  }

  /// Whether a 403 body's `detail` carries the denial reason (default `false`: reasons can
  /// reveal how access is decided).
  #[must_use]
  pub fn expose_reason(mut self, expose: bool) -> Self {
    self.expose_reason = expose;
    self
  }

  /// The guard's policy name.
  pub fn name(&self) -> &str {
    &self.name
  }

  /// Decides for the request head `parts`: `Ok(())` when allowed, otherwise the error to answer
  /// with.
  pub async fn decide(&self, parts: &Parts) -> Result<(), AuthzError> {
    self.decide_for(parts, false).await
  }

  /// [`decide`](Guard::decide); with `signed_in_only` the anonymous principal is denied without
  /// evaluating the policy (the decision is still recorded to [`Guard::events`]).
  async fn decide_for(&self, parts: &Parts, signed_in_only: bool) -> Result<(), AuthzError> {
    let principal = principal_of(parts);
    if self.events.is_none() {
      // A pure policy decides here, without a fiber, as the effect below would.
      let now = if signed_in_only && !principal.is_authenticated() {
        Some(Decision::Deny(Denial::new("authentication required")))
      } else {
        (self.now)(&principal, parts)
      };
      if let Some(decision) = now {
        return match decision {
          Decision::Allow => Ok(()),
          Decision::Deny(_) if !principal.is_authenticated() => Err(AuthzError::Unauthenticated),
          Decision::Deny(denial) => Err(AuthzError::Denied(denial.attributed_to(&self.name))),
        };
      }
    }
    let mut effect = if signed_in_only && !principal.is_authenticated() {
      id_effect::succeed(Decision::Deny(Denial::new("authentication required")))
    } else {
      (self.check)(&principal, parts)
    };
    if let Some(events) = &self.events {
      let resource = format!("{} {}", parts.method, parts.uri.path());
      effect = audited(effect, events, &principal, &self.name, &resource);
    }
    let name = Arc::clone(&self.name);
    let effect = effect.flat_map(move |decision| match decision {
      Decision::Deny(denial) => fail(AuthzError::Denied(denial.attributed_to(&name))),
      Decision::Allow => id_effect::succeed(()),
    });
    match EffectExecutor::global().run_effect(effect).await {
      Exit::Success(()) => Ok(()),
      Exit::Failure(cause) => Err(first_error(cause, &principal)),
    }
  }

  /// The response for `error` raised by this guard.
  pub fn reject(&self, error: AuthzError) -> Response {
    problem(error, Some(&self.name), self.expose_reason)
  }
}

/// The guard of a typed route (`Route::guard`, `.authorize(..)` on an endpoint): the anonymous
/// principal is denied (**401**) without evaluating the policy, a decision recorded like any
/// other; any other principal is [decided](Guard::decide) and a denial answered with
/// [`Guard::reject`].
impl RouteGuard for Guard {
  fn policy(&self) -> &str {
    &self.name
  }

  fn check<'a>(&'a self, parts: &'a Parts) -> GuardFuture<'a> {
    Box::pin(async move {
      self
        .decide_for(parts, true)
        .await
        .map_err(|error| self.reject(error))
    })
  }
}

impl fmt::Debug for Guard {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("Guard")
      .field("name", &self.name)
      .field("events", &self.events.is_some())
      .field("expose_reason", &self.expose_reason)
      .finish()
  }
}

/// The principal of a request (see the [module docs](self)): the [`CurrentPrincipal`] extension,
/// else one recorded in [`RequestCaps`], else anonymous.
pub fn principal_of(parts: &Parts) -> CurrentPrincipal {
  if let Some(principal) = parts.extensions.get::<CurrentPrincipal>() {
    return principal.clone();
  }
  parts
    .extensions
    .get::<RequestCaps>()
    .and_then(RequestCaps::get::<CurrentPrincipal>)
    .map_or_else(CurrentPrincipal::anonymous, |p| CurrentPrincipal::clone(&p))
}

/// The typed failure in `cause` mapped to what the guard answers: a denial of an anonymous
/// principal is `Unauthenticated`; a defect or interrupt is an `Evaluation` error.
fn first_error(cause: Cause<AuthzError>, principal: &CurrentPrincipal) -> AuthzError {
  match cause {
    Cause::Fail(AuthzError::Denied(_)) if !principal.is_authenticated() => {
      AuthzError::Unauthenticated
    }
    Cause::Fail(error) => error,
    other => AuthzError::Evaluation(format!("{other:?}")),
  }
}

fn problem(error: AuthzError, policy: Option<&str>, expose_reason: bool) -> Response {
  let status =
    StatusCode::from_u16(error.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
  let detail = match &error {
    AuthzError::Unauthenticated => "authentication required".to_owned(),
    AuthzError::Denied(denial) if expose_reason => denial.reason().to_owned(),
    AuthzError::Denied(_) => "access denied".to_owned(),
    _ => "authorization could not be evaluated".to_owned(),
  };
  let policy = policy.or(match &error {
    AuthzError::Denied(denial) => denial.policy(),
    _ => None,
  });
  let mut body = json!({
    "type": "about:blank",
    "title": status.canonical_reason().unwrap_or("Error"),
    "status": status.as_u16(),
    "detail": detail,
  });
  if let Some(policy) = policy {
    body["policy"] = json!(policy);
  }
  let mut response = (status, body.to_string()).into_response();
  response.headers_mut().insert(
    header::CONTENT_TYPE,
    HeaderValue::from_static("application/problem+json"),
  );
  response.extensions_mut().insert(error);
  response
}

/// Answers per [`AuthzError::http_status`] with a Problem Details body (the denial reason is not
/// included), so a handler can fail with `AuthzError` directly.
impl IntoResponse for AuthzError {
  fn into_response(self) -> Response {
    problem(self, None, false)
  }
}

impl<S> Layer<S> for Guard {
  type Service = GuardService<S>;

  fn layer(&self, inner: S) -> Self::Service {
    GuardService {
      guard: self.clone(),
      inner,
    }
  }
}

/// The service a [`Guard`] wraps a route in.
#[derive(Clone, Debug)]
pub struct GuardService<S> {
  guard: Guard,
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
    let guard = self.guard.clone();
    Box::pin(async move {
      let (parts, body) = req.into_parts();
      match guard.decide(&parts).await {
        Ok(()) => inner.call(Request::from_parts(parts, body)).await,
        Err(error) => Ok(guard.reject(error)),
      }
    })
  }
}

/// `.authorize(guard)` on routes: an Axum [`MethodRouter`] or a typed
/// [`Endpoint`].
pub trait AuthorizeExt: Sized {
  /// The guarded route.
  type Output;

  /// Evaluate `guard` before this route's handler.
  fn authorize(self, guard: Guard) -> Self::Output;
}

impl<S: Clone + Send + Sync + 'static> AuthorizeExt for MethodRouter<S> {
  type Output = MethodRouter<S>;

  fn authorize(self, guard: Guard) -> MethodRouter<S> {
    self.layer(guard)
  }
}

/// Guards a typed endpoint of any access state: the endpoint becomes
/// [`Guarded`] (see [`Endpoint::guard`]).
impl<S: Clone + Send + Sync + 'static, A> AuthorizeExt for Endpoint<S, A> {
  type Output = AuthorizedEndpoint<S>;

  fn authorize(self, guard: Guard) -> AuthorizedEndpoint<S> {
    AuthorizedEndpoint {
      endpoint: self.guard(guard),
    }
  }
}

impl<S: Clone + Send + Sync + 'static> AuthorizeExt for AuthorizedEndpoint<S> {
  type Output = AuthorizedEndpoint<S>;

  fn authorize(self, guard: Guard) -> AuthorizedEndpoint<S> {
    AuthorizedEndpoint {
      endpoint: self.endpoint.guard(guard),
    }
  }
}

/// A typed endpoint behind one or more guards: its [`RouteInfo`] (whose
/// [`policies`](RouteInfo::policies) lists the guard names) and the guarded method router.
///
/// Add it to an [`id_effect_axum::route::Router`] through [`AuthorizedEndpoint::into_endpoint`]
/// (or `Endpoint::from`), mount it with [`AuthorizedEndpoint::mount`] (both keep the
/// [`RouteRegistry`] complete) or take [`AuthorizedEndpoint::into_parts`].
#[must_use = "mount the endpoint on a router"]
pub struct AuthorizedEndpoint<S = ()> {
  endpoint: Endpoint<S, Guarded>,
}

impl<S: Clone + Send + Sync + 'static> AuthorizedEndpoint<S> {
  /// What the route declares.
  pub fn info(&self) -> &RouteInfo {
    self.endpoint.info()
  }

  /// The guards' policy names, outermost last ([`RouteInfo::policies`]).
  pub fn policies(&self) -> &[String] {
    &self.endpoint.info().policies
  }

  /// The guarded route as a typed, [`Guarded`] [`Endpoint`], for
  /// [`id_effect_axum::route::Router::route`].
  pub fn into_endpoint(self) -> Endpoint<S, Guarded> {
    self.endpoint
  }

  /// The declaration and the guarded method router.
  pub fn into_parts(self) -> (RouteInfo, MethodRouter<S>) {
    self.endpoint.into_parts()
  }

  /// Route the endpoint on `router` at its path and record its [`RouteInfo`] in `registry`
  /// (from [`id_effect_axum::route::Router::into_parts`]).
  ///
  /// # Panics
  ///
  /// Like [`axum::Router::route`]: when the method is already routed at that path.
  pub fn mount(self, router: axum::Router<S>, registry: &mut RouteRegistry) -> axum::Router<S> {
    let (info, method_router) = self.into_parts();
    let router = router.route(&info.path, method_router);
    registry.register(info);
    router
  }
}

impl<S: Clone + Send + Sync + 'static> From<AuthorizedEndpoint<S>> for Endpoint<S, Guarded> {
  fn from(endpoint: AuthorizedEndpoint<S>) -> Self {
    endpoint.into_endpoint()
  }
}

impl<S> fmt::Debug for AuthorizedEndpoint<S> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let info = self.endpoint.info();
    f.debug_struct("AuthorizedEndpoint")
      .field("method", &info.method)
      .field("path", &info.path)
      .field("policies", &info.policies)
      .finish()
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::audit::{MemorySink, Outcome};
  use crate::policy::{Denial, allow_all, deny_all, from_effect, require_role};
  use crate::principal::BasicPrincipal;

  fn head(principal: Option<CurrentPrincipal>) -> Parts {
    let mut req = Request::get("/books/7").body(()).expect("request");
    if let Some(p) = principal {
      req.extensions_mut().insert(p);
    }
    req.into_parts().0
  }

  fn alice() -> CurrentPrincipal {
    CurrentPrincipal::new(BasicPrincipal::new("alice").with_role("editor"))
  }

  #[test]
  fn principal_comes_from_the_extension_then_request_caps() {
    assert_eq!(principal_of(&head(Some(alice()))).id(), "alice");
    assert!(!principal_of(&head(None)).is_authenticated());
    let mut parts = head(None);
    let mut caps = RequestCaps::new();
    caps.insert(alice());
    parts.extensions.insert(caps);
    assert_eq!(principal_of(&parts).id(), "alice");
  }

  #[tokio::test]
  async fn decisions_map_to_errors() {
    let editor = Guard::new("edit", require_role("editor"));
    assert_eq!(editor.decide(&head(Some(alice()))).await, Ok(()));
    assert_eq!(
      editor.decide(&head(None)).await,
      Err(AuthzError::Unauthenticated)
    );
    let closed = Guard::new("closed", deny_all("maintenance"));
    assert_eq!(
      closed.decide(&head(Some(alice()))).await,
      Err(AuthzError::Denied(
        Denial::new("maintenance").attributed_to("closed")
      ))
    );
    let broken = Guard::new(
      "broken",
      from_effect(|_: &CurrentPrincipal, _: &()| -> PolicyEffect { id_effect::die("boom") }),
    );
    assert!(matches!(
      broken.decide(&head(Some(alice()))).await,
      Err(AuthzError::Evaluation(_))
    ));
    let unknown = Guard::named(&Policies::default(), "nope");
    assert_eq!(
      unknown.decide(&head(Some(alice()))).await,
      Err(AuthzError::UnknownPolicy("nope".into()))
    );
  }

  #[tokio::test]
  async fn request_guards_see_the_head_and_record_events() {
    let sink = MemorySink::new();
    let books = Guard::request(
      "books",
      crate::policy::predicate("not a book", |_, parts: &Parts| {
        parts.uri.path().starts_with("/books/")
      }),
    )
    .events(AuthzEvents::new(sink.clone()));
    assert_eq!(books.decide(&head(Some(alice()))).await, Ok(()));
    let events = sink.events();
    assert_eq!(events[0].resource, "GET /books/7");
    assert_eq!(events[0].outcome, Outcome::Allow);
    assert_eq!(events[0].policy, "books");
  }

  #[test]
  fn guarded_endpoints_record_their_policies_in_the_route_info() {
    use id_effect_axum::route::{Input, Route, Router};
    let endpoint = Route::get("/books")
      .handler(|_: &mut (), _: Input| id_effect::succeed::<_, Infallible, ()>("books"))
      .authorize(Guard::new("books.read", allow_all()))
      .authorize(Guard::new("staff", allow_all()));
    assert_eq!(endpoint.info().policies, ["books.read", "staff"]);
    let router = Router::new().route(endpoint.into());
    let info = router.routes().next().expect("registered");
    assert_eq!(info.policies, ["books.read", "staff"]);
    let plain = Route::get("/open")
      .handler(|_: &mut (), _: Input| id_effect::succeed::<_, Infallible, ()>("open"));
    assert!(plain.info().policies.is_empty());
    let guarded = Route::get("/books")
      .guard(Guard::new("books.read", allow_all()))
      .handler(|_: &mut (), _: Input| id_effect::succeed::<_, Infallible, ()>("books"));
    assert_eq!(guarded.info().policies, ["books.read"]);
  }

  #[tokio::test]
  async fn h8_anonymous_reaches_only_public_routes() {
    use id_effect_axum::route::{Input, Route, Router};
    use tower::ServiceExt;

    fn ok(_: &mut (), _: Input) -> id_effect::Effect<&'static str, Infallible, ()> {
      id_effect::succeed("ok")
    }

    // `x-user` stands in for an authentication layer: it records alice; without it the request
    // continues as the anonymous principal.
    let signed_in = id_effect_axum::CapOverrideLayer::new(|parts: &Parts| {
      parts.headers.get("x-user").map(|_| alice())
    });
    let app = Router::new()
      .route(Route::get("/public").public().handler(ok))
      // A policy admitting everyone still does not admit the anonymous principal.
      .route(
        Route::get("/open")
          .guard(Guard::new("open", allow_all()))
          .handler(ok),
      )
      .route(
        Route::get("/edit")
          .handler(ok)
          .authorize(Guard::new("edit", require_role("editor")))
          .into_endpoint(),
      )
      .into_axum()
      .route("/plain", axum::routing::get(|| async { "plain" }))
      .layer(signed_in);
    let call = |uri: &str, user: bool| {
      let mut req = Request::get(uri);
      if user {
        req = req.header("x-user", "alice");
      }
      app
        .clone()
        .oneshot(req.body(axum::body::Body::empty()).expect("request"))
    };

    assert_eq!(
      call("/public", false).await.unwrap().status(),
      StatusCode::OK
    );
    for uri in ["/open", "/edit"] {
      let res = call(uri, false).await.unwrap();
      assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "{uri}");
      assert_eq!(
        res.headers()[header::CONTENT_TYPE],
        "application/problem+json"
      );
      assert_eq!(
        res.extensions().get::<AuthzError>(),
        Some(&AuthzError::Unauthenticated)
      );
      assert_eq!(
        call(uri, true).await.unwrap().status(),
        StatusCode::OK,
        "{uri}"
      );
    }
    // Plain routes merged after `into_axum` are outside the typed router's decision.
    assert_eq!(
      call("/plain", false).await.unwrap().status(),
      StatusCode::OK
    );
  }

  #[test]
  fn problem_bodies_and_debug() {
    let guard = Guard::new("p", allow_all()).expose_reason(true);
    let res = guard.reject(AuthzError::Denied(Denial::new("why")));
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    assert_eq!(
      res.headers()[header::CONTENT_TYPE],
      "application/problem+json"
    );
    assert!(res.extensions().get::<AuthzError>().is_some());
    let res = AuthzError::Evaluation("db".into()).into_response();
    assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let dbg = format!("{guard:?}");
    assert!(dbg.contains("expose_reason: true"), "{dbg}");
    assert_eq!(guard.name(), "p");
  }
}
