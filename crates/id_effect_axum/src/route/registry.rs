//! The route registry: what every typed [`Route`](super::Route) declares, kept for introspection.
//!
//! Each [`Endpoint`](super::Endpoint) carries a [`RouteInfo`] — method, path template and the
//! [`SchemaRef`] of its path parameters, query, headers, body and documented responses. A
//! [`Router`](super::Router) collects them in a [`RouteRegistry`] that OpenAPI generation and a
//! `/_routes` listing read; [`SchemaRef::json_schema`] emits the JSON Schema (draft 2020-12) of
//! any part.

use axum::http::{Method, StatusCode};
use id_effect::schema::{HasSchema, Schema, SchemaAst, ast_to_json_schema};
use serde_json::Value;

/// Type-erased handle to a [`HasSchema`] type's schema: its Rust type name and wire-shape
/// [`SchemaAst`].
#[derive(Clone, Debug, PartialEq)]
pub struct SchemaRef {
  type_name: &'static str,
  ast: SchemaAst,
}

impl SchemaRef {
  /// Handle for `T` (builds [`HasSchema::schema`] once).
  pub fn of<T: HasSchema>() -> Self {
    Self::from_schema(std::any::type_name::<T>(), &T::schema())
  }

  /// Handle for an existing `schema`, described as `type_name`.
  pub fn from_schema<A, I, E>(type_name: &'static str, schema: &Schema<A, I, E>) -> Self {
    Self {
      type_name,
      ast: schema.ast().clone(),
    }
  }

  /// Full Rust type name ([`std::any::type_name`]) of the declaring type; descriptive only.
  pub fn type_name(&self) -> &'static str {
    self.type_name
  }

  /// Last path segment of [`SchemaRef::type_name`] (`my_app::AccountId` → `AccountId`), generic
  /// arguments included.
  pub fn short_name(&self) -> &'static str {
    let base = self
      .type_name
      .split_once('<')
      .map_or(self.type_name, |(head, _)| head);
    let start = base.rfind("::").map_or(0, |i| i + 2);
    &self.type_name[start..]
  }

  /// Introspectable wire shape.
  pub fn ast(&self) -> &SchemaAst {
    &self.ast
  }

  /// JSON Schema (draft 2020-12) document for this part.
  pub fn json_schema(&self) -> Value {
    ast_to_json_schema(&self.ast)
  }
}

/// One documented response of a route: status and body schema.
#[derive(Clone, Debug, PartialEq)]
pub struct ResponseInfo {
  /// Response status.
  pub status: StatusCode,
  /// Response body schema.
  pub schema: SchemaRef,
}

/// How a typed route reads its request body.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum BodyKind {
  /// A JSON document ([`Route::body`](super::Route::body)).
  #[default]
  Json,
  /// Form fields: `application/x-www-form-urlencoded` or the text fields of
  /// `multipart/form-data` ([`Route::form`](super::Route::form)).
  Form,
}

impl BodyKind {
  /// Request media types this body is read from.
  pub fn media_types(self) -> &'static [&'static str] {
    match self {
      BodyKind::Json => &["application/json"],
      BodyKind::Form => &["application/x-www-form-urlencoded", "multipart/form-data"],
    }
  }
}

/// Who may call a route: the access decision every typed route makes (ADR 0013 R1).
///
/// [`Router::route`](super::Router::route) only accepts [`Public`](RouteAccess::Public) and
/// [`Guarded`](RouteAccess::Guarded) endpoints; the deny-by-default layer of
/// [`Router::into_axum`](super::Router::into_axum) answers 404 for every other route.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum RouteAccess {
  /// No decision yet ([`Route::public`](super::Route::public) or
  /// [`Route::guard`](super::Route::guard) not called); such a route cannot be routed.
  #[default]
  Undecided,
  /// Anyone may call the route, anonymous callers included.
  Public,
  /// Guards decide before the handler runs; their policy names are in
  /// [`RouteInfo::policies`].
  Guarded,
  /// Routed without a decision through the deprecated
  /// [`Router::route_unchecked`](super::Router::route_unchecked) (a recorded opt-out).
  Unchecked,
}

impl RouteAccess {
  /// Lower-case name (`"public"`, `"guarded"`, `"unchecked"`, `"undecided"`).
  pub fn as_str(self) -> &'static str {
    match self {
      RouteAccess::Undecided => "undecided",
      RouteAccess::Public => "public",
      RouteAccess::Guarded => "guarded",
      RouteAccess::Unchecked => "unchecked",
    }
  }

  /// Whether the deny-by-default layer serves a route with this access.
  pub fn is_served(self) -> bool {
    !matches!(self, RouteAccess::Undecided)
  }
}

/// Everything a typed route declares. Equality compares the declaration, not
/// [`handler`](RouteInfo::handler).
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct RouteInfo {
  /// HTTP method.
  pub method: Method,
  /// Path template in Axum syntax (`/accounts/{id}`), which is also OpenAPI's.
  pub path: String,
  /// Rust type name ([`std::any::type_name`]) of the handler given to
  /// [`Route::handler`](super::Route::handler) (`my_app::show_account`; a closure reads
  /// `my_app::routes::{{closure}}`); `None` for a [`RouteInfo::new`] not built by a route.
  /// Descriptive only, for route listings: not compared by `==`.
  pub handler: Option<&'static str>,
  /// Path parameter schema, when declared with [`Route::path`](super::Route::path).
  pub path_params: Option<SchemaRef>,
  /// Query string schema, when declared with [`Route::query`](super::Route::query).
  pub query: Option<SchemaRef>,
  /// Request header schema, when declared with [`Route::headers`](super::Route::headers).
  pub headers: Option<SchemaRef>,
  /// Request body schema, when declared with [`Route::body`](super::Route::body) or
  /// [`Route::form`](super::Route::form).
  pub body: Option<SchemaRef>,
  /// How the body is read ([`BodyKind::Json`] unless declared with
  /// [`Route::form`](super::Route::form)).
  pub body_kind: BodyKind,
  /// Documented responses, in declaration order ([`Route::response`](super::Route::response)).
  pub responses: Vec<ResponseInfo>,
  /// Names of the authorization policies guarding the route, outermost last (empty when the
  /// route is unguarded); filled by [`Route::guard`](super::Route::guard) and guards such as
  /// `id_effect_authz`'s `.authorize(..)`.
  pub policies: Vec<String>,
  /// The route's access decision: [`RouteAccess::Public`], or [`RouteAccess::Guarded`] by
  /// [`policies`](RouteInfo::policies).
  pub access: RouteAccess,
}

impl RouteInfo {
  /// A route with no declared parts and no access decision ([`RouteAccess::Undecided`]).
  pub fn new(method: Method, path: impl Into<String>) -> Self {
    Self {
      method,
      path: path.into(),
      handler: None,
      path_params: None,
      query: None,
      headers: None,
      body: None,
      body_kind: BodyKind::Json,
      responses: Vec::new(),
      policies: Vec::new(),
      access: RouteAccess::Undecided,
    }
  }
}

impl PartialEq for RouteInfo {
  fn eq(&self, other: &Self) -> bool {
    let Self {
      method,
      path,
      handler: _,
      path_params,
      query,
      headers,
      body,
      body_kind,
      responses,
      policies,
      access,
    } = self;
    *method == other.method
      && *path == other.path
      && *path_params == other.path_params
      && *query == other.query
      && *headers == other.headers
      && *body == other.body
      && *body_kind == other.body_kind
      && *responses == other.responses
      && *policies == other.policies
      && *access == other.access
  }
}

/// Every [`RouteInfo`] registered on a [`Router`](super::Router), in registration order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RouteRegistry {
  routes: Vec<RouteInfo>,
}

impl RouteRegistry {
  /// Empty registry.
  pub fn new() -> Self {
    Self::default()
  }

  /// Record `info`.
  pub fn register(&mut self, info: RouteInfo) {
    self.routes.push(info);
  }

  /// Registered routes in registration order.
  pub fn iter(&self) -> std::slice::Iter<'_, RouteInfo> {
    self.routes.iter()
  }

  /// Number of registered routes.
  pub fn len(&self) -> usize {
    self.routes.len()
  }

  /// `true` when nothing is registered.
  pub fn is_empty(&self) -> bool {
    self.routes.is_empty()
  }

  /// The route registered for `method` and path template `path`, if any.
  pub fn find(&self, method: &Method, path: &str) -> Option<&RouteInfo> {
    self
      .routes
      .iter()
      .find(|r| r.method == *method && r.path == path)
  }
}

impl<'a> IntoIterator for &'a RouteRegistry {
  type Item = &'a RouteInfo;
  type IntoIter = std::slice::Iter<'a, RouteInfo>;

  fn into_iter(self) -> Self::IntoIter {
    self.iter()
  }
}

impl IntoIterator for RouteRegistry {
  type Item = RouteInfo;
  type IntoIter = std::vec::IntoIter<RouteInfo>;

  fn into_iter(self) -> Self::IntoIter {
    self.routes.into_iter()
  }
}

impl Extend<RouteInfo> for RouteRegistry {
  fn extend<T: IntoIterator<Item = RouteInfo>>(&mut self, iter: T) {
    self.routes.extend(iter);
  }
}
