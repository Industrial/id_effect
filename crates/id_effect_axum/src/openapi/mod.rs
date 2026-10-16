//! OpenAPI 3.1 documents generated from typed routes (feature **`openapi`**).
//!
//! [`OpenApi::from_router`] walks a [`Router`]'s [`RouteRegistry`] and describes every typed
//! route as an operation:
//!
//! - **Schemas are the decoding schemas.** Each [`HasSchema`](id_effect::schema::HasSchema) type a
//!   route declares as its body or a response becomes one entry of `components/schemas` (named
//!   after the type, `$ref`-deduplicated) whose value is the type's
//!   [`json_schema_for`](id_effect::schema::json_schema_for) document — the JSON Schema
//!   (draft 2020-12, OpenAPI 3.1's dialect) of the very schema the extractors decode with. Named
//!   recursive definitions (`$defs`) move to components of their own and the `$ref`s follow them.
//! - **Parameters.** Every `{name}` of the path template is a required path parameter, typed by
//!   the declared path schema; query and header struct schemas give one parameter per field.
//! - **Request bodies** list the media types of the route's [`BodyKind`](crate::route::BodyKind).
//! - **Responses**: each [`Route::response`](crate::route::Route::response) under
//!   `application/json`, plus the [`Problem`](crate::Problem) responses the route can answer
//!   (`application/problem+json`): **422** when it declares input, **400** for a JSON body,
//!   **401**/**403** when guarded.
//! - **Security.** Schemes declared with [`OpenApi::security_scheme`] (bearer JWT, session
//!   cookie, OpenID Connect, …) appear under `components/securitySchemes`; every route guarded by
//!   policies ([`RouteInfo::policies`](crate::route::RouteInfo::policies)) requires one of them,
//!   and lists its policies as `x-policies`.
//!
//! The document is plain serde data: adjust any [`Operation`] through [`OpenApi::operation_mut`]
//! and serve [`OpenApi::to_json`].
//!
//! ```rust
//! use axum::http::{Method, StatusCode};
//! use id_effect::schema::{self, HasSchema, Schema};
//! use id_effect::{Effect, succeed};
//! use id_effect_axum::openapi::{Info, OpenApi};
//! use id_effect_axum::route::{Input, Route, Router};
//!
//! struct Account;
//!
//! impl HasSchema for Account {
//!   type A = (String, i64);
//!   type I = (String, i64);
//!   type E = ();
//!
//!   fn schema() -> Schema<Self::A, Self::I> {
//!     schema::struct_("owner", schema::string(), "balance", schema::i64())
//!   }
//! }
//!
//! type Body = Input<(), (), (), (String, i64)>;
//!
//! fn create(_: &mut (), input: Body) -> Effect<String, std::convert::Infallible, ()> {
//!   succeed(input.body.0)
//! }
//!
//! let router = Router::new().route(
//!   Route::post("/accounts")
//!     .body::<Account>()
//!     .response::<Account>(StatusCode::CREATED)
//!     .handler(create),
//! );
//! let doc = OpenApi::from_router(&router)
//!   .info(Info::new("Ledger", "1.0.0"))
//!   .bearer_jwt("bearer");
//! let json = doc.to_json();
//! assert_eq!(json["openapi"], "3.1.0");
//! assert_eq!(
//!   json["components"]["schemas"]["Account"],
//!   schema::json_schema_for::<Account>()
//! );
//! let op = doc.operation(&Method::POST, "/accounts").unwrap();
//! assert!(op.responses.contains_key("422"));
//! ```

mod build;
mod model;

use std::collections::BTreeMap;

use axum::http::Method;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use model::{
  ApiKeyLocation, Components, Info, MediaType, Operation, Parameter, ParameterIn, PathItem,
  RequestBody, Response, SecurityRequirement, SecurityScheme, Server, Tag,
};

use crate::route::{RouteInfo, RouteRegistry, Router};

/// The OpenAPI version the documents declare.
pub const OPENAPI_VERSION: &str = "3.1.0";

/// An OpenAPI 3.1 document (the OpenAPI Object); build it with [`OpenApi::from_router`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct OpenApi {
  /// OpenAPI version ([`OPENAPI_VERSION`]).
  pub openapi: String,
  /// Title, version and description of the API.
  pub info: Info,
  /// Base URLs.
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub servers: Vec<Server>,
  /// Operations by OpenAPI path (`/accounts/{id}`).
  #[serde(default)]
  pub paths: BTreeMap<String, PathItem>,
  /// Component schemas and security schemes.
  #[serde(default, skip_serializing_if = "Components::is_empty")]
  pub components: Components,
  /// Tag descriptions.
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub tags: Vec<Tag>,
}

impl Default for OpenApi {
  fn default() -> Self {
    Self::new()
  }
}

impl OpenApi {
  /// A document without operations, titled `API` at version `0.0.0`.
  pub fn new() -> Self {
    Self {
      openapi: OPENAPI_VERSION.to_owned(),
      info: Info::default(),
      servers: Vec::new(),
      paths: BTreeMap::new(),
      components: Components::default(),
      tags: Vec::new(),
    }
  }

  /// Document every typed route of `router`.
  pub fn from_router<S>(router: &Router<S>) -> Self
  where
    S: Clone + Send + Sync + 'static,
  {
    Self::from_registry(router.registry())
  }

  /// Document every route of `registry`.
  pub fn from_registry(registry: &RouteRegistry) -> Self {
    Self::from_routes(registry)
  }

  /// Document `routes` (later routes replace earlier ones with the same method and path; methods
  /// OpenAPI cannot describe, such as `CONNECT`, are skipped).
  pub fn from_routes<'a>(routes: impl IntoIterator<Item = &'a RouteInfo>) -> Self {
    let built = build::build(routes);
    Self {
      paths: built.paths,
      components: built.components,
      ..Self::new()
    }
  }

  /// Replace the [`Info`].
  #[must_use]
  pub fn info(mut self, info: Info) -> Self {
    self.info = info;
    self
  }

  /// Set the API title.
  #[must_use]
  pub fn title(mut self, title: impl Into<String>) -> Self {
    self.info.title = title.into();
    self
  }

  /// Set the API version.
  #[must_use]
  pub fn version(mut self, version: impl Into<String>) -> Self {
    self.info.version = version.into();
    self
  }

  /// Set the API description (CommonMark).
  #[must_use]
  pub fn description(mut self, description: impl Into<String>) -> Self {
    self.info.description = Some(description.into());
    self
  }

  /// Add a server.
  #[must_use]
  pub fn server(mut self, server: Server) -> Self {
    self.servers.push(server);
    self
  }

  /// Add (or replace, by name) a tag description.
  #[must_use]
  pub fn tag(mut self, tag: Tag) -> Self {
    self.tags.retain(|t| t.name != tag.name);
    self.tags.push(tag);
    self
  }

  /// Declare the security scheme `name` and accept it on every operation guarded by policies
  /// (one more alternative in its `security`). For OpenID Connect the requirement lists no
  /// scopes; for other schemes it lists the operation's policy names as role names.
  /// Re-declaring a name replaces the scheme.
  #[must_use]
  pub fn security_scheme(mut self, name: impl Into<String>, scheme: SecurityScheme) -> Self {
    let name = name.into();
    for op in self.operations_mut() {
      if op.policies.is_empty() {
        continue;
      }
      let roles = if scheme.takes_scopes() {
        Vec::new()
      } else {
        op.policies.clone()
      };
      let security = op.security.get_or_insert_with(Vec::new);
      security.retain(|requirement| !requirement.contains_key(&name));
      security.push(BTreeMap::from([(name.clone(), roles)]));
    }
    self.components.security_schemes.insert(name, scheme);
    self
  }

  /// [`SecurityScheme::bearer_jwt`] as `name`.
  #[must_use]
  pub fn bearer_jwt(self, name: impl Into<String>) -> Self {
    self.security_scheme(name, SecurityScheme::bearer_jwt())
  }

  /// [`SecurityScheme::cookie_session`] (cookie `cookie`) as `name`.
  #[must_use]
  pub fn cookie_session(self, name: impl Into<String>, cookie: impl Into<String>) -> Self {
    self.security_scheme(name, SecurityScheme::cookie_session(cookie))
  }

  /// [`SecurityScheme::openid_connect`] (discovery document at `discovery_url`) as `name`.
  #[must_use]
  pub fn openid_connect(self, name: impl Into<String>, discovery_url: impl Into<String>) -> Self {
    self.security_scheme(name, SecurityScheme::openid_connect(discovery_url))
  }

  /// The operation for `method` at OpenAPI path `path` (`/files/{rest}` for the Axum template
  /// `/files/{*rest}`).
  pub fn operation(&self, method: &Method, path: &str) -> Option<&Operation> {
    self.paths.get(path)?.operation(method)
  }

  /// Mutable access to an operation (to add a summary, description, …).
  pub fn operation_mut(&mut self, method: &Method, path: &str) -> Option<&mut Operation> {
    self.paths.get_mut(path)?.operation_mut(method)
  }

  /// Every operation of the document.
  pub fn operations_mut(&mut self) -> impl Iterator<Item = &mut Operation> {
    self.paths.values_mut().flat_map(PathItem::operations_mut)
  }

  /// The document as JSON (what `/openapi.json` serves).
  pub fn to_json(&self) -> Value {
    serde_json::to_value(self).expect("an OpenAPI document serializes to JSON")
  }

  /// The document as pretty-printed JSON text.
  pub fn to_json_pretty(&self) -> String {
    serde_json::to_string_pretty(self).expect("an OpenAPI document serializes to JSON")
  }
}
