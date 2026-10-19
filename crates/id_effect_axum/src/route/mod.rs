//! Typed routes: declare a route's path parameters, query, headers, body and responses as
//! [`HasSchema`] types, and keep every declaration in a [`RouteRegistry`].
//!
//! [`Route::get`] (and siblings) start a builder; [`Route::path`], [`Route::query`],
//! [`Route::headers`] and [`Route::body`] (or [`Route::form`]) each name a [`HasSchema`] type whose
//! decoded value the handler receives in [`Input`]; [`Route::response`] documents a response.
//! [`Route::handler`] takes an ordinary effect handler — `Fn(&mut S, Input<…>) -> Effect<A, E, S>` —
//! run exactly like the [`crate::routing`] helpers (one request fiber on the global
//! [`crate::EffectExecutor`], exit mapped by [`crate::exit_response`]). Input that does not decode
//! never reaches the handler: it is answered with an RFC 9457 [`Problem`]
//! (`application/problem+json`, **422** — **400** for a malformed JSON body) listing every
//! [`Issue`] of every part at once, each with a JSON Pointer.
//!
//! Plain Axum handlers get the same decoding per part from the [`extract`] extractors
//! ([`ValidPath`], [`ValidQuery`], [`ValidHeaders`], [`ValidJson`], [`ValidForm`]).
//!
//! Every route declares who may call it before a [`Router`] accepts it (deny by default, see
//! [`access`]): [`Route::public`] for anyone, anonymous callers included, or [`Route::guard`]
//! for a [`RouteGuard`] that decides first. A route with neither does not compile.
//!
//! A [`Router`] wraps an [`axum::Router`] and records each route's [`RouteInfo`] (method, path
//! template, access, [`SchemaRef`] per part) for OpenAPI generation and route listings. The
//! existing [`crate::routing`] helpers are unchanged and can be mixed in through
//! [`Router::into_axum`].
//!
//! ```rust
//! use axum::http::StatusCode;
//! use id_effect::schema::{self, HasSchema, Schema};
//! use id_effect::{Effect, succeed};
//! use id_effect_axum::route::{Input, Route, Router};
//!
//! struct AccountId;
//!
//! impl HasSchema for AccountId {
//!   type A = i64;
//!   type I = i64;
//!   type E = ();
//!
//!   fn schema() -> Schema<i64, i64> {
//!     schema::i64::<()>()
//!   }
//! }
//!
//! #[derive(Clone)]
//! struct App;
//!
//! fn show(_: &mut App, input: Input<i64>) -> Effect<String, std::convert::Infallible, App> {
//!   succeed(format!("account {}", input.path))
//! }
//!
//! let router = Router::new().route(
//!   Route::get("/accounts/{id}")
//!     .public()
//!     .path::<AccountId>()
//!     .response::<AccountId>(StatusCode::OK)
//!     .handler(show),
//! );
//! let info = router.routes().next().unwrap();
//! assert_eq!(info.path, "/accounts/{id}");
//! assert_eq!(router.access_report().public, ["GET /accounts/{id}"]);
//! assert!(info.path_params.is_some());
//! let app: axum::Router = router.with_state(App).into_axum();
//! # let _ = app;
//! ```

pub mod access;
mod decode;
pub mod extract;
#[cfg(feature = "openapi")]
pub mod introspect;
pub mod problem;
mod registry;

use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{MethodFilter, MethodRouter};
use id_effect::Effect;
use id_effect::schema::{HasSchema, ParseError, ParseErrors, Schema, Unknown};

use crate::request_caps::RequestCaps;

pub use access::{
  Access, AccessLayer, AccessReport, AccessService, Decided, GuardFuture, Guarded, Public,
  RouteGuard, Undecided,
};
use access::{GuardLayer, Guards};
pub use decode::RouteRejection;
pub use extract::{ValidForm, ValidHeaders, ValidJson, ValidPath, ValidQuery};
pub use problem::{Issue, Location, PROBLEM_JSON, Problem};
pub use registry::{BodyKind, ResponseInfo, RouteAccess, RouteInfo, RouteRegistry, SchemaRef};

mod sealed {
  pub trait Sealed {}
}

/// One request part of a [`Route`]: [`Undeclared`] or [`Declared`]. Sealed.
pub trait RoutePart: sealed::Sealed + Send + Sync + 'static {
  /// What the handler receives for this part in [`Input`].
  type Value: Send + 'static;

  /// The declared schema, if any.
  fn schema(&self) -> Option<&SchemaRef>;

  /// Decode this part from `input` (`None` when the part is undeclared), collecting every issue.
  fn decode(&self, input: Option<&Unknown>) -> Result<Self::Value, ParseErrors>;
}

/// A request part the route does not declare; the handler receives `()` for it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Undeclared;

impl sealed::Sealed for Undeclared {}

impl RoutePart for Undeclared {
  type Value = ();

  fn schema(&self) -> Option<&SchemaRef> {
    None
  }

  fn decode(&self, _input: Option<&Unknown>) -> Result<(), ParseErrors> {
    Ok(())
  }
}

/// A request part declared as the [`HasSchema`] type `T`; the handler receives `T::A`.
pub struct Declared<T: HasSchema> {
  schema: Schema<T::A, T::I, T::E>,
  info: SchemaRef,
  _type: PhantomData<fn() -> T>,
}

impl<T: HasSchema> Declared<T> {
  fn new() -> Self {
    let schema = T::schema();
    let info = SchemaRef::from_schema(std::any::type_name::<T>(), &schema);
    Self {
      schema,
      info,
      _type: PhantomData,
    }
  }
}

impl<T: HasSchema> Clone for Declared<T> {
  fn clone(&self) -> Self {
    Self {
      schema: self.schema.clone(),
      info: self.info.clone(),
      _type: PhantomData,
    }
  }
}

impl<T: HasSchema> std::fmt::Debug for Declared<T> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_tuple("Declared").field(&self.info).finish()
  }
}

impl<T: HasSchema> sealed::Sealed for Declared<T> {}

impl<T> RoutePart for Declared<T>
where
  T: HasSchema + 'static,
  T::A: Send,
{
  type Value = T::A;

  fn schema(&self) -> Option<&SchemaRef> {
    Some(&self.info)
  }

  fn decode(&self, input: Option<&Unknown>) -> Result<T::A, ParseErrors> {
    match input {
      Some(u) => self.schema.decode_unknown_all(u),
      None => Err(ParseErrors::one(ParseError::new("", "missing input"))),
    }
  }
}

/// The decoded input a typed handler receives; undeclared parts are `()`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Input<P = (), Q = (), H = (), B = ()> {
  /// Path parameters ([`Route::path`]).
  pub path: P,
  /// Query string ([`Route::query`]).
  pub query: Q,
  /// Request headers ([`Route::headers`]).
  pub headers: H,
  /// Request body ([`Route::body`] or [`Route::form`]).
  pub body: B,
}

/// The four request parts of a route, shared by every request of its handler.
#[derive(Clone, Debug)]
pub(crate) struct Parts<P, Q, H, B> {
  pub(crate) path: P,
  pub(crate) query: Q,
  pub(crate) headers: H,
  pub(crate) body: B,
  pub(crate) body_kind: BodyKind,
}

/// Builder for one typed route; decide its access with [`Route::public`] or [`Route::guard`]
/// and finish it with [`Route::handler`].
///
/// `A` is the access state: [`Undecided`] until one of the two is called, then [`Public`] or
/// [`Guarded`]; [`Router::route`] only accepts the endpoint of a decided route.
#[derive(Clone, Debug)]
#[must_use = "a route does nothing until `.handler(f)` is added to a `Router`"]
pub struct Route<P = Undeclared, Q = Undeclared, H = Undeclared, B = Undeclared, A = Undecided> {
  method: Method,
  template: String,
  parts: Parts<P, Q, H, B>,
  responses: Vec<ResponseInfo>,
  guards: Guards,
  access: PhantomData<A>,
}

impl Route {
  /// Route for `method` at `path` (Axum template syntax, e.g. `/accounts/{id}`).
  pub fn new(method: Method, path: impl Into<String>) -> Self {
    Self {
      method,
      template: path.into(),
      parts: Parts {
        path: Undeclared,
        query: Undeclared,
        headers: Undeclared,
        body: Undeclared,
        body_kind: BodyKind::Json,
      },
      responses: Vec::new(),
      guards: Guards::default(),
      access: PhantomData,
    }
  }

  /// `GET` route.
  pub fn get(path: impl Into<String>) -> Self {
    Self::new(Method::GET, path)
  }

  /// `POST` route.
  pub fn post(path: impl Into<String>) -> Self {
    Self::new(Method::POST, path)
  }

  /// `PUT` route.
  pub fn put(path: impl Into<String>) -> Self {
    Self::new(Method::PUT, path)
  }

  /// `PATCH` route.
  pub fn patch(path: impl Into<String>) -> Self {
    Self::new(Method::PATCH, path)
  }

  /// `DELETE` route.
  pub fn delete(path: impl Into<String>) -> Self {
    Self::new(Method::DELETE, path)
  }
}

impl<P, Q, H, B> Route<P, Q, H, B, Undecided> {
  /// Anyone may call this route, anonymous callers included. Listed by
  /// [`Router::access_report`] and documented with no security requirement.
  pub fn public(self) -> Route<P, Q, H, B, Public> {
    Route {
      method: self.method,
      template: self.template,
      parts: self.parts,
      responses: self.responses,
      guards: self.guards,
      access: PhantomData,
    }
  }
}

impl<P, Q, H, B, A> Route<P, Q, H, B, A> {
  /// Evaluate `guard` before the handler (repeatable; each later guard wraps the earlier ones,
  /// outermost last). Its [`policy`](RouteGuard::policy) is recorded in
  /// [`RouteInfo::policies`]. On a public route it makes the route guarded.
  pub fn guard(mut self, guard: impl RouteGuard) -> Route<P, Q, H, B, Guarded> {
    self.guards.0.push(Arc::new(guard));
    Route {
      method: self.method,
      template: self.template,
      parts: self.parts,
      responses: self.responses,
      guards: self.guards,
      access: PhantomData,
    }
  }

  /// Declare the path parameters as `T`: a struct schema keyed by parameter name, or any scalar
  /// schema when the template has exactly one parameter.
  pub fn path<T>(self) -> Route<Declared<T>, Q, H, B, A>
  where
    T: HasSchema + 'static,
    T::A: Send,
  {
    let Parts {
      query,
      headers,
      body,
      body_kind,
      ..
    } = self.parts;
    Route {
      method: self.method,
      template: self.template,
      parts: Parts {
        path: Declared::new(),
        query,
        headers,
        body,
        body_kind,
      },
      responses: self.responses,
      guards: self.guards,
      access: PhantomData,
    }
  }

  /// Declare the query string as `T` (a struct or record schema; an array field collects
  /// repeated keys and is `[]` when the key is absent, a nullable field is `null` when absent).
  pub fn query<T>(self) -> Route<P, Declared<T>, H, B, A>
  where
    T: HasSchema + 'static,
    T::A: Send,
  {
    let Parts {
      path,
      headers,
      body,
      body_kind,
      ..
    } = self.parts;
    Route {
      method: self.method,
      template: self.template,
      parts: Parts {
        path,
        query: Declared::new(),
        headers,
        body,
        body_kind,
      },
      responses: self.responses,
      guards: self.guards,
      access: PhantomData,
    }
  }

  /// Declare request headers as `T` (a struct schema whose field names are header names, matched
  /// case-insensitively).
  pub fn headers<T>(self) -> Route<P, Q, Declared<T>, B, A>
  where
    T: HasSchema + 'static,
    T::A: Send,
  {
    let Parts {
      path,
      query,
      body,
      body_kind,
      ..
    } = self.parts;
    Route {
      method: self.method,
      template: self.template,
      parts: Parts {
        path,
        query,
        headers: Declared::new(),
        body,
        body_kind,
      },
      responses: self.responses,
      guards: self.guards,
      access: PhantomData,
    }
  }

  /// Declare the JSON request body as `T` (the `Content-Type` is not checked; a body that is not
  /// JSON answers **400**).
  pub fn body<T>(self) -> Route<P, Q, H, Declared<T>, A>
  where
    T: HasSchema + 'static,
    T::A: Send,
  {
    self.with_body(BodyKind::Json)
  }

  /// Declare a form request body as `T`, decoded like [`Route::query`]: the fields of an
  /// `application/x-www-form-urlencoded` body, or the text fields of a `multipart/form-data` body
  /// (file fields are skipped). Another content type keeps Axum's `415` rejection.
  pub fn form<T>(self) -> Route<P, Q, H, Declared<T>, A>
  where
    T: HasSchema + 'static,
    T::A: Send,
  {
    self.with_body(BodyKind::Form)
  }

  fn with_body<T>(self, body_kind: BodyKind) -> Route<P, Q, H, Declared<T>, A>
  where
    T: HasSchema + 'static,
    T::A: Send,
  {
    let Parts {
      path,
      query,
      headers,
      ..
    } = self.parts;
    Route {
      method: self.method,
      template: self.template,
      parts: Parts {
        path,
        query,
        headers,
        body: Declared::new(),
        body_kind,
      },
      responses: self.responses,
      guards: self.guards,
      access: PhantomData,
    }
  }

  /// Document a `status` response whose body is `T` (repeatable). Documentation only: the
  /// handler's success value still renders through [`IntoResponse`].
  pub fn response<T: HasSchema>(mut self, status: StatusCode) -> Self {
    self.responses.push(ResponseInfo {
      status,
      schema: SchemaRef::of::<T>(),
    });
    self
  }
}

/// Boxed future of a typed handler.
type HandlerFuture = Pin<Box<dyn Future<Output = Response> + Send>>;

impl<P, Q, H, B, A> Route<P, Q, H, B, A>
where
  P: RoutePart,
  Q: RoutePart,
  H: RoutePart,
  B: RoutePart,
  A: Access,
{
  /// Everything this route declares.
  pub fn info(&self) -> RouteInfo {
    let mut info = RouteInfo::new(self.method.clone(), self.template.clone());
    info.path_params = self.parts.path.schema().cloned();
    info.query = self.parts.query.schema().cloned();
    info.headers = self.parts.headers.schema().cloned();
    info.body = self.parts.body.schema().cloned();
    info.body_kind = self.parts.body_kind;
    info.responses = self.responses.clone();
    info.policies = self.guards.policies().collect();
    info.access = A::ACCESS;
    info
  }

  /// Finish the route with an effect handler.
  ///
  /// Per request the declared parts are decoded (all issues collected; a failure answers a
  /// [`Problem`] without running the handler), then `f(&mut state, input)` builds the
  /// effect, which runs as one request fiber like the [`crate::routing`] helpers: success → `A`,
  /// typed failure → `E`, defect → `500`, interrupt or saturation → `503`.
  ///
  /// # Panics
  ///
  /// If Axum cannot route the method (an extension method such as `PROPFIND`).
  pub fn handler<S, T, E, F>(self, f: F) -> Endpoint<S, A>
  where
    S: Clone + Send + Sync + 'static,
    T: IntoResponse + Send + 'static,
    E: IntoResponse + Send + 'static,
    F: Fn(&mut S, Input<P::Value, Q::Value, H::Value, B::Value>) -> Effect<T, E, S>
      + Clone
      + Send
      + Sync
      + 'static,
  {
    self.endpoint(std::any::type_name::<F>(), move |s, _caps, input| {
      f(s, input)
    })
  }

  /// [`handler`](Self::handler) for a handler that also reads the request's [`RequestCaps`]:
  /// the per-request capabilities a [`CapOverrideLayer`](crate::CapOverrideLayer) or an
  /// authentication layer recorded (the caller's principal, its tenant), empty when there are
  /// none. The handler builds the request's environment from them, as
  /// [`execute_with_request_caps`](crate::execute_with_request_caps) does for plain handlers.
  ///
  /// # Panics
  ///
  /// If Axum cannot route the method (an extension method such as `PROPFIND`).
  pub fn handler_with_request_caps<S, T, E, F>(self, f: F) -> Endpoint<S, A>
  where
    S: Clone + Send + Sync + 'static,
    T: IntoResponse + Send + 'static,
    E: IntoResponse + Send + 'static,
    F: Fn(&mut S, RequestCaps, Input<P::Value, Q::Value, H::Value, B::Value>) -> Effect<T, E, S>
      + Clone
      + Send
      + Sync
      + 'static,
  {
    self.endpoint(std::any::type_name::<F>(), f)
  }

  /// The endpoint running `f` (named `handler` in the [`RouteInfo`]) for each decoded request.
  fn endpoint<S, T, E, F>(self, handler: &'static str, f: F) -> Endpoint<S, A>
  where
    S: Clone + Send + Sync + 'static,
    T: IntoResponse + Send + 'static,
    E: IntoResponse + Send + 'static,
    F: Fn(&mut S, RequestCaps, Input<P::Value, Q::Value, H::Value, B::Value>) -> Effect<T, E, S>
      + Clone
      + Send
      + Sync
      + 'static,
  {
    let mut info = self.info();
    info.handler = Some(handler);
    let filter = MethodFilter::try_from(self.method.clone())
      .unwrap_or_else(|e| panic!("typed route {} {}: {e}", self.method, self.template));
    let parts = Arc::new(self.parts);
    let guards = self.guards;
    let mut method_router = axum::routing::on(filter, move |State(env): State<S>, req: Request| {
      let parts = Arc::clone(&parts);
      let f = f.clone();
      Box::pin(async move {
        let caps = req
          .extensions()
          .get::<RequestCaps>()
          .cloned()
          .unwrap_or_default();
        match decode::decode_input(&parts, req).await {
          Ok(input) => {
            crate::respond(crate::try_run_effect_from_state(env, move |e| f(e, caps, input)).await)
          }
          Err(rejection) => *rejection,
        }
      }) as HandlerFuture
    });
    for guard in guards.0 {
      method_router = method_router.layer(GuardLayer(guard));
    }
    Endpoint {
      info,
      method_router,
      access: PhantomData,
    }
  }
}

/// A finished typed route: its [`RouteInfo`] and the Axum [`MethodRouter`] serving it.
///
/// `A` is the route's access state ([`Undecided`], [`Public`] or [`Guarded`]);
/// [`Router::route`] accepts only a decided endpoint.
#[must_use = "add the endpoint to a `Router`"]
pub struct Endpoint<S = (), A = Undecided> {
  info: RouteInfo,
  method_router: MethodRouter<S>,
  access: PhantomData<A>,
}

impl<S, A> Endpoint<S, A> {
  /// What the route declares.
  pub fn info(&self) -> &RouteInfo {
    &self.info
  }

  /// The declaration and the method router (to mount it on a plain [`axum::Router`]).
  pub fn into_parts(self) -> (RouteInfo, MethodRouter<S>) {
    (self.info, self.method_router)
  }
}

impl<S> Endpoint<S, Undecided> {
  /// Reassemble an endpoint from [`Endpoint::into_parts`] (e.g. after wrapping the method router),
  /// or make one from a plain Axum `method_router` and a [`RouteInfo::new`]. The endpoint is
  /// [`Undecided`] whatever `info.access` says: decide again with [`Endpoint::public`] or
  /// [`Endpoint::guard`].
  pub fn from_parts(mut info: RouteInfo, method_router: MethodRouter<S>) -> Self {
    info.access = RouteAccess::Undecided;
    Self {
      info,
      method_router,
      access: PhantomData,
    }
  }

  /// Anyone may call this endpoint, anonymous callers included (like [`Route::public`]).
  pub fn public(mut self) -> Endpoint<S, Public> {
    self.info.access = RouteAccess::Public;
    Endpoint {
      info: self.info,
      method_router: self.method_router,
      access: PhantomData,
    }
  }
}

impl<S: Clone + Send + Sync + 'static, A> Endpoint<S, A> {
  /// Evaluate `guard` before the handler and any layer added so far (like [`Route::guard`]):
  /// its policy is appended to [`RouteInfo::policies`] and the endpoint becomes [`Guarded`].
  pub fn guard(mut self, guard: impl RouteGuard) -> Endpoint<S, Guarded> {
    self.info.policies.push(guard.policy().to_owned());
    self.info.access = RouteAccess::Guarded;
    Endpoint {
      info: self.info,
      method_router: self.method_router.layer(GuardLayer(Arc::new(guard))),
      access: PhantomData,
    }
  }

  /// Wrap the handler in `layer` (see [`MethodRouter::layer`]); the [`RouteInfo`] is unchanged.
  pub fn layer<L>(self, layer: L) -> Self
  where
    L: tower_layer::Layer<axum::routing::Route> + Clone + Send + Sync + 'static,
    L::Service: tower_service::Service<Request> + Clone + Send + Sync + 'static,
    <L::Service as tower_service::Service<Request>>::Response: IntoResponse + 'static,
    <L::Service as tower_service::Service<Request>>::Error:
      Into<std::convert::Infallible> + 'static,
    <L::Service as tower_service::Service<Request>>::Future: Send + 'static,
  {
    Self {
      info: self.info,
      method_router: self.method_router.layer(layer),
      access: PhantomData,
    }
  }
}

impl<S, A> std::fmt::Debug for Endpoint<S, A> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("Endpoint")
      .field("method", &self.info.method)
      .field("path", &self.info.path)
      .field("access", &self.info.access)
      .field("policies", &self.info.policies)
      .finish()
  }
}

/// An [`axum::Router`] that records the [`RouteInfo`] of every typed route added to it, and
/// serves only routes that declare their access (see [`access`]).
#[must_use]
pub struct Router<S = ()> {
  inner: axum::Router<S>,
  registry: RouteRegistry,
  allow_unregistered: bool,
}

impl<S: Clone + Send + Sync + 'static> Default for Router<S> {
  fn default() -> Self {
    Self::new()
  }
}

impl<S: Clone + Send + Sync + 'static> Router<S> {
  /// Empty router.
  pub fn new() -> Self {
    Self {
      inner: axum::Router::new(),
      registry: RouteRegistry::new(),
      allow_unregistered: false,
    }
  }

  /// Mount `endpoint` at its path and register its [`RouteInfo`]. Several methods may share a
  /// path.
  ///
  /// Only a [`Decided`] endpoint compiles: call [`Route::public`] or [`Route::guard`] (or
  /// `.authorize(guard)` from `id_effect_authz`) first.
  ///
  /// # Panics
  ///
  /// Like [`axum::Router::route`]: when the method is already routed at that path, or the path is
  /// not a valid template.
  pub fn route<A: Decided>(self, endpoint: Endpoint<S, A>) -> Self {
    self.mount(endpoint.info, endpoint.method_router)
  }

  /// Mount `endpoint` without an access decision: the 0.10 behaviour, which serves every caller,
  /// anonymous ones included. An undecided endpoint is recorded as [`RouteAccess::Unchecked`]
  /// and the opt-out `id_effect_axum::route::Router::route_unchecked` in the
  /// [posture registry](id_effect::observability::posture). Removed in 0.12.
  ///
  /// # Panics
  ///
  /// Like [`Router::route`].
  #[deprecated(
    since = "0.11.0",
    note = "declare access with `Route::public()` or `Route::guard(..)` and use `Router::route`; `route_unchecked` is removed in 0.12"
  )]
  pub fn route_unchecked<A: Access>(self, endpoint: Endpoint<S, A>) -> Self {
    let mut info = endpoint.info;
    if info.access == RouteAccess::Undecided {
      id_effect::observability::posture::record_dangerous(
        "id_effect_axum::route::Router::route_unchecked",
        "H8: a typed route serves every caller, anonymous ones included, without an access decision",
      );
      info.access = RouteAccess::Unchecked;
    }
    self.mount(info, endpoint.method_router)
  }

  fn mount(mut self, info: RouteInfo, method_router: MethodRouter<S>) -> Self {
    self.inner = self.inner.route(&info.path, method_router);
    self.registry.register(info);
    self
  }

  /// Merge `other`'s routes and registry into this router (see [`axum::Router::merge`]).
  pub fn merge(mut self, other: Router<S>) -> Self {
    self.inner = self.inner.merge(other.inner);
    self.registry.extend(other.registry);
    self.allow_unregistered |= other.allow_unregistered;
    self
  }

  /// Merge the plain Axum router `other` (see [`axum::Router::merge`]). Its routes declare no
  /// access, so they answer **404** Problem Details once [`Router::into_axum`] installs the
  /// deny-by-default layer — unless this router
  /// [allows unregistered routes](Router::allow_unregistered_routes_dangerous). To serve a plain
  /// handler, make it an endpoint and decide:
  /// `Endpoint::from_parts(RouteInfo::new(method, path), method_router).public()`.
  pub fn merge_axum(mut self, other: axum::Router<S>) -> Self {
    self.inner = self.inner.merge(other);
    self
  }

  /// Serve routes merged with [`Router::merge_axum`] although they declare no access: turns the
  /// deny-by-default layer off. Records the opt-out
  /// `id_effect_axum::route::Router::allow_unregistered_routes_dangerous` in the
  /// [posture registry](id_effect::observability::posture).
  pub fn allow_unregistered_routes_dangerous(mut self) -> Self {
    id_effect::observability::posture::record_dangerous(
      "id_effect_axum::route::Router::allow_unregistered_routes_dangerous",
      "H8: plain routes without an access decision are served to every caller",
    );
    self.allow_unregistered = true;
    self
  }

  /// Who may call each route: public routes, guarded routes with their policies, unchecked
  /// routes and whether unregistered routes are allowed. [`Router::into_axum`] logs it
  /// (`tracing`, target `id_effect_axum::route::access`).
  pub fn access_report(&self) -> AccessReport {
    AccessReport::new(&self.registry, self.allow_unregistered)
  }

  /// The deny-by-default layer for this router's routes (see [`AccessLayer`]); add it last with
  /// `route_layer` to cover plain routes merged onto [`Router::into_axum`]'s result.
  pub fn access_layer(&self) -> AccessLayer {
    AccessLayer::new(&self.registry)
  }

  /// Every registered route, in registration order.
  pub fn routes(&self) -> std::slice::Iter<'_, RouteInfo> {
    self.registry.iter()
  }

  /// The route registry.
  pub fn registry(&self) -> &RouteRegistry {
    &self.registry
  }

  /// Provide the state, keeping the registry (see [`axum::Router::with_state`]).
  pub fn with_state<S2>(self, state: S) -> Router<S2>
  where
    S2: Clone + Send + Sync + 'static,
  {
    Router {
      inner: self.inner.with_state(state),
      registry: self.registry,
      allow_unregistered: self.allow_unregistered,
    }
  }

  /// The Axum router, with the deny-by-default layer installed, and the registry.
  pub fn into_parts(self) -> (axum::Router<S>, RouteRegistry) {
    let inner = self.deny_unregistered();
    (inner, self.registry)
  }

  /// The Axum router (add plain routes, layers, or serve it), with the deny-by-default
  /// [`AccessLayer`] on every route merged so far: a route that is not a registered public or
  /// guarded typed route answers **404** Problem Details, unless
  /// [`Router::allow_unregistered_routes_dangerous`] was taken. Logs the
  /// [`access_report`](Router::access_report).
  ///
  /// Routes added to the returned router are outside the layer; add
  /// [`Router::access_layer`] last to cover them.
  pub fn into_axum(self) -> axum::Router<S> {
    self.deny_unregistered()
  }

  fn deny_unregistered(&self) -> axum::Router<S> {
    self.access_report().log();
    if self.allow_unregistered || !self.inner.has_routes() {
      self.inner.clone()
    } else {
      self.inner.clone().route_layer(self.access_layer())
    }
  }
}

impl<S: Clone + Send + Sync + 'static> From<Router<S>> for axum::Router<S> {
  fn from(router: Router<S>) -> Self {
    router.into_axum()
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect::schema;

  struct Id;

  impl HasSchema for Id {
    type A = i64;
    type I = i64;
    type E = ();

    fn schema() -> Schema<i64, i64> {
      schema::i64::<()>()
    }
  }

  #[test]
  fn routes_clone_and_debug_and_declared_parts_need_input() {
    let route = Route::get("/ids/{id}").path::<Id>();
    let copy = route.clone();
    assert_eq!(copy.info(), route.info());
    assert!(format!("{route:?}").contains("Declared"));

    let declared = Declared::<Id>::new();
    assert_eq!(declared.decode(Some(&Unknown::I64(3))).unwrap(), 3);
    let missing = declared.decode(None).unwrap_err();
    assert_eq!(missing.issues[0].message, "missing input");
    assert!(Undeclared.decode(None).is_ok());
  }

  #[tokio::test]
  async fn endpoints_take_layers_and_reassemble_from_parts() {
    use tower::ServiceExt;
    fn ok(_: &mut (), _: Input) -> Effect<&'static str, std::convert::Infallible, ()> {
      id_effect::succeed("ok")
    }
    let stamp = tower::util::MapResponseLayer::new(|mut res: Response| {
      res
        .headers_mut()
        .insert("x-layered", axum::http::HeaderValue::from_static("1"));
      res
    });
    let endpoint = Route::get("/x").public().handler(ok).layer(stamp);
    assert_eq!(endpoint.info().access, RouteAccess::Public);
    let (info, method_router) = endpoint.into_parts();
    // A reassembled endpoint decides again.
    let endpoint = Endpoint::from_parts(info, method_router);
    assert_eq!(endpoint.info().access, RouteAccess::Undecided);
    assert!(format!("{endpoint:?}").contains("Undecided"));
    let router = Router::new().route(endpoint.public());
    assert_eq!(router.routes().next().unwrap().access, RouteAccess::Public);
    let req = axum::http::Request::get("/x")
      .body(axum::body::Body::empty())
      .unwrap();
    let res = router.into_axum().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()["x-layered"], "1");
  }

  #[tokio::test]
  async fn request_caps_reach_the_typed_handler() {
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    #[derive(Clone)]
    struct Tenant(String);

    fn whoami(_: &mut (), caps: RequestCaps, input: Input<i64>) -> Effect<String, Problem, ()> {
      let tenant = caps
        .get::<Tenant>()
        .map_or_else(|| "none".to_owned(), |t| t.0.clone());
      id_effect::succeed(format!("{tenant}:{}", input.path))
    }

    let endpoint = Route::get("/ids/{id}")
      .public()
      .path::<Id>()
      .handler_with_request_caps(whoami);
    assert!(endpoint.info().handler.unwrap().contains("whoami"));
    let app = Router::new()
      .route(endpoint)
      .into_axum()
      .layer(crate::CapOverrideLayer::new(
        |parts: &axum::http::request::Parts| {
          let tenant = parts.headers.get("x-tenant")?.to_str().ok()?;
          Some(Tenant(tenant.to_owned()))
        },
      ));
    let call = |tenant: Option<&str>, path: &str| {
      let mut req = axum::http::Request::get(path);
      if let Some(tenant) = tenant {
        req = req.header("x-tenant", tenant);
      }
      app
        .clone()
        .oneshot(req.body(axum::body::Body::empty()).unwrap())
    };
    let text = |res: Response| async move {
      let bytes = res.into_body().collect().await.unwrap().to_bytes();
      String::from_utf8(bytes.to_vec()).unwrap()
    };
    let res = call(Some("acme"), "/ids/7").await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(text(res).await, "acme:7");
    let res = call(None, "/ids/8").await.unwrap();
    assert_eq!(text(res).await, "none:8");
    let res = call(Some("acme"), "/ids/x").await.unwrap();
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
  }
}
