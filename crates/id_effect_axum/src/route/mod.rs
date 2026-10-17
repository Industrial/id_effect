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
//! A [`Router`] wraps an [`axum::Router`] and records each route's [`RouteInfo`] (method, path
//! template, [`SchemaRef`] per part) for OpenAPI generation and route listings. The existing
//! [`crate::routing`] helpers are unchanged and can be mixed in through [`Router::into_axum`].
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
//!     .path::<AccountId>()
//!     .response::<AccountId>(StatusCode::OK)
//!     .handler(show),
//! );
//! let info = router.routes().next().unwrap();
//! assert_eq!(info.path, "/accounts/{id}");
//! assert!(info.path_params.is_some());
//! let app: axum::Router = router.with_state(App).into_axum();
//! # let _ = app;
//! ```

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

pub use decode::RouteRejection;
pub use extract::{ValidForm, ValidHeaders, ValidJson, ValidPath, ValidQuery};
pub use problem::{Issue, Location, PROBLEM_JSON, Problem};
pub use registry::{BodyKind, ResponseInfo, RouteInfo, RouteRegistry, SchemaRef};

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

/// Builder for one typed route; finish it with [`Route::handler`].
#[derive(Clone, Debug)]
#[must_use = "a route does nothing until `.handler(f)` is added to a `Router`"]
pub struct Route<P = Undeclared, Q = Undeclared, H = Undeclared, B = Undeclared> {
  method: Method,
  template: String,
  parts: Parts<P, Q, H, B>,
  responses: Vec<ResponseInfo>,
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

impl<P, Q, H, B> Route<P, Q, H, B> {
  /// Declare the path parameters as `T`: a struct schema keyed by parameter name, or any scalar
  /// schema when the template has exactly one parameter.
  pub fn path<T>(self) -> Route<Declared<T>, Q, H, B>
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
    }
  }

  /// Declare the query string as `T` (a struct or record schema; an array field collects
  /// repeated keys and is `[]` when the key is absent, a nullable field is `null` when absent).
  pub fn query<T>(self) -> Route<P, Declared<T>, H, B>
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
    }
  }

  /// Declare request headers as `T` (a struct schema whose field names are header names, matched
  /// case-insensitively).
  pub fn headers<T>(self) -> Route<P, Q, Declared<T>, B>
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
    }
  }

  /// Declare the JSON request body as `T` (the `Content-Type` is not checked; a body that is not
  /// JSON answers **400**).
  pub fn body<T>(self) -> Route<P, Q, H, Declared<T>>
  where
    T: HasSchema + 'static,
    T::A: Send,
  {
    self.with_body(BodyKind::Json)
  }

  /// Declare a form request body as `T`, decoded like [`Route::query`]: the fields of an
  /// `application/x-www-form-urlencoded` body, or the text fields of a `multipart/form-data` body
  /// (file fields are skipped). Another content type keeps Axum's `415` rejection.
  pub fn form<T>(self) -> Route<P, Q, H, Declared<T>>
  where
    T: HasSchema + 'static,
    T::A: Send,
  {
    self.with_body(BodyKind::Form)
  }

  fn with_body<T>(self, body_kind: BodyKind) -> Route<P, Q, H, Declared<T>>
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

impl<P, Q, H, B> Route<P, Q, H, B>
where
  P: RoutePart,
  Q: RoutePart,
  H: RoutePart,
  B: RoutePart,
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
  pub fn handler<S, A, E, F>(self, f: F) -> Endpoint<S>
  where
    S: Clone + Send + Sync + 'static,
    A: IntoResponse + Send + 'static,
    E: IntoResponse + Send + 'static,
    F: Fn(&mut S, Input<P::Value, Q::Value, H::Value, B::Value>) -> Effect<A, E, S>
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
  pub fn handler_with_request_caps<S, A, E, F>(self, f: F) -> Endpoint<S>
  where
    S: Clone + Send + Sync + 'static,
    A: IntoResponse + Send + 'static,
    E: IntoResponse + Send + 'static,
    F: Fn(&mut S, RequestCaps, Input<P::Value, Q::Value, H::Value, B::Value>) -> Effect<A, E, S>
      + Clone
      + Send
      + Sync
      + 'static,
  {
    self.endpoint(std::any::type_name::<F>(), f)
  }

  /// The endpoint running `f` (named `handler` in the [`RouteInfo`]) for each decoded request.
  fn endpoint<S, A, E, F>(self, handler: &'static str, f: F) -> Endpoint<S>
  where
    S: Clone + Send + Sync + 'static,
    A: IntoResponse + Send + 'static,
    E: IntoResponse + Send + 'static,
    F: Fn(&mut S, RequestCaps, Input<P::Value, Q::Value, H::Value, B::Value>) -> Effect<A, E, S>
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
    let method_router = axum::routing::on(filter, move |State(env): State<S>, req: Request| {
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
    Endpoint {
      info,
      method_router,
    }
  }
}

/// A finished typed route: its [`RouteInfo`] and the Axum [`MethodRouter`] serving it.
#[must_use = "add the endpoint to a `Router`"]
pub struct Endpoint<S = ()> {
  info: RouteInfo,
  method_router: MethodRouter<S>,
}

impl<S> Endpoint<S> {
  /// What the route declares.
  pub fn info(&self) -> &RouteInfo {
    &self.info
  }

  /// The declaration and the method router (to mount it on a plain [`axum::Router`]).
  pub fn into_parts(self) -> (RouteInfo, MethodRouter<S>) {
    (self.info, self.method_router)
  }

  /// Reassemble an endpoint from [`Endpoint::into_parts`] (e.g. after wrapping the method router
  /// or recording [`RouteInfo::policies`]).
  pub fn from_parts(info: RouteInfo, method_router: MethodRouter<S>) -> Self {
    Self {
      info,
      method_router,
    }
  }
}

impl<S: Clone + Send + Sync + 'static> Endpoint<S> {
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
    }
  }
}

/// An [`axum::Router`] that records the [`RouteInfo`] of every typed route added to it.
#[must_use]
pub struct Router<S = ()> {
  inner: axum::Router<S>,
  registry: RouteRegistry,
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
    }
  }

  /// Mount `endpoint` at its path and register its [`RouteInfo`]. Several methods may share a
  /// path.
  ///
  /// # Panics
  ///
  /// Like [`axum::Router::route`]: when the method is already routed at that path, or the path is
  /// not a valid template.
  pub fn route(mut self, endpoint: Endpoint<S>) -> Self {
    let (info, method_router) = endpoint.into_parts();
    self.inner = self.inner.route(&info.path, method_router);
    self.registry.register(info);
    self
  }

  /// Merge `other`'s routes and registry into this router (see [`axum::Router::merge`]).
  pub fn merge(mut self, other: Router<S>) -> Self {
    self.inner = self.inner.merge(other.inner);
    self.registry.extend(other.registry);
    self
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
    }
  }

  /// The underlying Axum router and the registry.
  pub fn into_parts(self) -> (axum::Router<S>, RouteRegistry) {
    (self.inner, self.registry)
  }

  /// The underlying Axum router (add plain routes, layers, or serve it).
  pub fn into_axum(self) -> axum::Router<S> {
    self.inner
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
    let endpoint = Route::get("/x").handler(ok).layer(stamp);
    let (mut info, method_router) = endpoint.into_parts();
    info.policies.push("staff".to_owned());
    let endpoint = Endpoint::from_parts(info, method_router);
    assert_eq!(endpoint.info().policies, ["staff"]);
    let router = Router::new().route(endpoint);
    assert_eq!(router.routes().next().unwrap().policies, ["staff"]);
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
