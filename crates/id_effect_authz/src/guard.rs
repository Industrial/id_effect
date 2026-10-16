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
use id_effect_axum::route::{Endpoint, RouteInfo, RouteRegistry};
use id_effect_axum::{EffectExecutor, RequestCaps};
use serde_json::json;
use tower_layer::Layer;
use tower_service::Service;

use crate::audit::{AuthzEvents, audited};
use crate::policy::{AuthzError, Decision, Policy, PolicyEffect};
use crate::principal::CurrentPrincipal;
use crate::registry::Policies;

type Check = dyn Fn(&CurrentPrincipal, &Parts) -> PolicyEffect + Send + Sync;

/// A route guard: a named policy evaluated for the request's principal before the handler.
///
/// It is a Tower [`Layer`]; attach it with [`AuthorizeExt::authorize`] or any `.layer(..)` /
/// `.route_layer(..)`.
#[derive(Clone)]
pub struct Guard {
  name: Arc<str>,
  check: Arc<Check>,
  events: Option<AuthzEvents>,
  expose_reason: bool,
}

impl Guard {
  /// A guard named `name` evaluating `policy` (a rule over the principal only).
  pub fn new(name: impl Into<String>, policy: impl Policy<()>) -> Self {
    Self::from_check(name, move |principal, _| policy.evaluate(principal, &()))
  }

  /// A guard named `name` evaluating `policy` against the request head (method, URI, headers,
  /// extensions) — e.g. a tenant taken from the path.
  pub fn request(name: impl Into<String>, policy: impl Policy<Parts>) -> Self {
    Self::from_check(name, move |principal, parts| {
      policy.evaluate(principal, parts)
    })
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
      events: None,
      expose_reason: false,
    }
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
    let principal = principal_of(parts);
    let mut effect = (self.check)(&principal, parts);
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

impl<S: Clone + Send + Sync + 'static> AuthorizeExt for Endpoint<S> {
  type Output = AuthorizedEndpoint<S>;

  fn authorize(self, guard: Guard) -> AuthorizedEndpoint<S> {
    let (info, method_router) = self.into_parts();
    AuthorizedEndpoint {
      info,
      method_router,
      policies: Vec::new(),
    }
    .authorize(guard)
  }
}

impl<S: Clone + Send + Sync + 'static> AuthorizeExt for AuthorizedEndpoint<S> {
  type Output = AuthorizedEndpoint<S>;

  fn authorize(mut self, guard: Guard) -> AuthorizedEndpoint<S> {
    self.policies.push(guard.name().to_owned());
    self.method_router = self.method_router.layer(guard);
    self
  }
}

/// A typed endpoint behind one or more guards: its [`RouteInfo`], the guard names, and the
/// guarded method router.
///
/// Mount it with [`AuthorizedEndpoint::mount`] (keeps the [`RouteRegistry`] complete) or
/// [`AuthorizedEndpoint::into_parts`].
#[must_use = "mount the endpoint on a router"]
pub struct AuthorizedEndpoint<S = ()> {
  info: RouteInfo,
  method_router: MethodRouter<S>,
  policies: Vec<String>,
}

impl<S: Clone + Send + Sync + 'static> AuthorizedEndpoint<S> {
  /// What the route declares.
  pub fn info(&self) -> &RouteInfo {
    &self.info
  }

  /// The guards' policy names, outermost last.
  pub fn policies(&self) -> &[String] {
    &self.policies
  }

  /// The declaration and the guarded method router.
  pub fn into_parts(self) -> (RouteInfo, MethodRouter<S>) {
    (self.info, self.method_router)
  }

  /// Route the endpoint on `router` at its path and record its [`RouteInfo`] in `registry`
  /// (from [`id_effect_axum::route::Router::into_parts`]).
  ///
  /// # Panics
  ///
  /// Like [`axum::Router::route`]: when the method is already routed at that path.
  pub fn mount(self, router: axum::Router<S>, registry: &mut RouteRegistry) -> axum::Router<S> {
    let router = router.route(&self.info.path, self.method_router);
    registry.register(self.info);
    router
  }
}

impl<S> fmt::Debug for AuthorizedEndpoint<S> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("AuthorizedEndpoint")
      .field("method", &self.info.method)
      .field("path", &self.info.path)
      .field("policies", &self.policies)
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
