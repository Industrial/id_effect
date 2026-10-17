//! The OpenAPI 3.1 document objects [`OpenApi`](super::OpenApi) is made of (serde, field names as
//! in the specification). Schema Objects are plain JSON ([`Value`]): JSON Schema draft 2020-12.

use std::collections::BTreeMap;

use axum::http::Method;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `true` for `false` (serde `skip_serializing_if` helper).
fn is_false(b: &bool) -> bool {
  !*b
}

/// A Security Requirement Object: security scheme name → scopes (OAuth2 / OpenID Connect) or
/// role names (other schemes).
pub type SecurityRequirement = BTreeMap<String, Vec<String>>;

/// Info Object: the API's title and version.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Info {
  /// Title of the API.
  pub title: String,
  /// Version of the API (not of OpenAPI).
  pub version: String,
  /// Short summary.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub summary: Option<String>,
  /// Longer description (CommonMark).
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub description: Option<String>,
}

impl Info {
  /// Info for `title` at API `version`.
  pub fn new(title: impl Into<String>, version: impl Into<String>) -> Self {
    Self {
      title: title.into(),
      version: version.into(),
      summary: None,
      description: None,
    }
  }

  /// With a short summary.
  #[must_use]
  pub fn with_summary(mut self, summary: impl Into<String>) -> Self {
    self.summary = Some(summary.into());
    self
  }

  /// With a description (CommonMark).
  #[must_use]
  pub fn with_description(mut self, description: impl Into<String>) -> Self {
    self.description = Some(description.into());
    self
  }
}

impl Default for Info {
  /// `API`, version `0.0.0`.
  fn default() -> Self {
    Self::new("API", "0.0.0")
  }
}

/// Server Object: a base URL the API is served from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Server {
  /// Base URL (may be relative, e.g. `/api`).
  pub url: String,
  /// What this server is.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub description: Option<String>,
}

impl Server {
  /// Server at `url`.
  pub fn new(url: impl Into<String>) -> Self {
    Self {
      url: url.into(),
      description: None,
    }
  }

  /// With a description.
  #[must_use]
  pub fn with_description(mut self, description: impl Into<String>) -> Self {
    self.description = Some(description.into());
    self
  }
}

/// Tag Object: a group of operations (each generated operation is tagged with the first literal
/// segment of its path).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Tag {
  /// Tag name.
  pub name: String,
  /// What the group is.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub description: Option<String>,
}

impl Tag {
  /// Tag `name`.
  pub fn new(name: impl Into<String>) -> Self {
    Self {
      name: name.into(),
      description: None,
    }
  }

  /// With a description.
  #[must_use]
  pub fn with_description(mut self, description: impl Into<String>) -> Self {
    self.description = Some(description.into());
    self
  }
}

/// Where a [`Parameter`] is read from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ParameterIn {
  /// A path template parameter (`{id}`).
  Path,
  /// A query string key.
  Query,
  /// A request header.
  Header,
  /// A cookie.
  Cookie,
}

/// Parameter Object.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Parameter {
  /// Parameter (path template variable, query key, header or cookie) name.
  pub name: String,
  /// Where it is read from.
  #[serde(rename = "in")]
  pub location: ParameterIn,
  /// Description, from the schema's `description`.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub description: Option<String>,
  /// Whether the request must carry it (always `true` for path parameters).
  #[serde(default, skip_serializing_if = "is_false")]
  pub required: bool,
  /// Deprecated, from the schema's `deprecated`.
  #[serde(default, skip_serializing_if = "is_false")]
  pub deprecated: bool,
  /// Serialization style (`form` for a free-form query object).
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub style: Option<String>,
  /// Whether arrays and objects explode into separate keys.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub explode: Option<bool>,
  /// The value's schema.
  pub schema: Value,
}

impl Parameter {
  /// Parameter `name` in `location` with `schema`; `description` and `deprecated` are taken from
  /// the schema's annotations.
  pub fn new(name: impl Into<String>, location: ParameterIn, schema: Value) -> Self {
    let description = schema
      .get("description")
      .and_then(Value::as_str)
      .map(str::to_owned);
    let deprecated = schema.get("deprecated") == Some(&Value::Bool(true));
    Self {
      name: name.into(),
      location,
      description,
      required: location == ParameterIn::Path,
      deprecated,
      style: None,
      explode: None,
      schema,
    }
  }

  /// With `required` set.
  #[must_use]
  pub fn with_required(mut self, required: bool) -> Self {
    self.required = required;
    self
  }
}

/// Media Type Object: the schema of one content type.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct MediaType {
  /// Body schema (a `$ref` into `components/schemas` for declared types).
  pub schema: Value,
}

impl MediaType {
  /// Content whose body is `schema`.
  pub fn new(schema: Value) -> Self {
    Self { schema }
  }
}

/// Request Body Object.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RequestBody {
  /// Description.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub description: Option<String>,
  /// Accepted media types.
  pub content: BTreeMap<String, MediaType>,
  /// Whether a body is required.
  #[serde(default, skip_serializing_if = "is_false")]
  pub required: bool,
}

/// Response Object.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Response {
  /// Description (required by OpenAPI; the status' reason phrase by default).
  pub description: String,
  /// Response media types (none for a body-less response).
  #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
  pub content: BTreeMap<String, MediaType>,
}

impl Response {
  /// Response described as `description`, without content.
  pub fn new(description: impl Into<String>) -> Self {
    Self {
      description: description.into(),
      content: BTreeMap::new(),
    }
  }

  /// With a `media_type` body of `schema`.
  #[must_use]
  pub fn with_content(mut self, media_type: impl Into<String>, schema: Value) -> Self {
    self
      .content
      .insert(media_type.into(), MediaType::new(schema));
    self
  }
}

/// Operation Object: one method on one path.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Operation {
  /// Groups (the first literal path segment).
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub tags: Vec<String>,
  /// Short summary.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub summary: Option<String>,
  /// Description (CommonMark).
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub description: Option<String>,
  /// Unique id (`get_accounts_by_id` for `GET /accounts/{id}`).
  pub operation_id: String,
  /// Path, query and header parameters.
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub parameters: Vec<Parameter>,
  /// The request body, when declared.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub request_body: Option<RequestBody>,
  /// Responses by status code (or `default`).
  pub responses: BTreeMap<String, Response>,
  /// Accepted security schemes (alternatives); `None` inherits the document's.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub security: Option<Vec<SecurityRequirement>>,
  /// Deprecated.
  #[serde(default, skip_serializing_if = "is_false")]
  pub deprecated: bool,
  /// The authorization policies guarding the route ([`RouteInfo::policies`]), as the
  /// `x-policies` extension.
  ///
  /// [`RouteInfo::policies`]: crate::route::RouteInfo::policies
  #[serde(rename = "x-policies", default, skip_serializing_if = "Vec::is_empty")]
  pub policies: Vec<String>,
}

/// Path Item Object: the operations of one path.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct PathItem {
  /// `GET`.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub get: Option<Operation>,
  /// `PUT`.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub put: Option<Operation>,
  /// `POST`.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub post: Option<Operation>,
  /// `DELETE`.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub delete: Option<Operation>,
  /// `OPTIONS`.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub options: Option<Operation>,
  /// `HEAD`.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub head: Option<Operation>,
  /// `PATCH`.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub patch: Option<Operation>,
  /// `TRACE`.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub trace: Option<Operation>,
}

impl PathItem {
  /// The slot for `method`; `None` for a method OpenAPI 3.1 cannot describe (`CONNECT`,
  /// extension methods).
  pub(crate) fn slot(&mut self, method: &Method) -> Option<&mut Option<Operation>> {
    let slot = match *method {
      Method::GET => &mut self.get,
      Method::PUT => &mut self.put,
      Method::POST => &mut self.post,
      Method::DELETE => &mut self.delete,
      Method::OPTIONS => &mut self.options,
      Method::HEAD => &mut self.head,
      Method::PATCH => &mut self.patch,
      Method::TRACE => &mut self.trace,
      _ => return None,
    };
    Some(slot)
  }

  /// The operation for `method`, if documented.
  pub fn operation(&self, method: &Method) -> Option<&Operation> {
    let slot = match *method {
      Method::GET => &self.get,
      Method::PUT => &self.put,
      Method::POST => &self.post,
      Method::DELETE => &self.delete,
      Method::OPTIONS => &self.options,
      Method::HEAD => &self.head,
      Method::PATCH => &self.patch,
      Method::TRACE => &self.trace,
      _ => return None,
    };
    slot.as_ref()
  }

  /// Mutable access to the operation for `method`, if documented.
  pub fn operation_mut(&mut self, method: &Method) -> Option<&mut Operation> {
    self.slot(method).and_then(Option::as_mut)
  }

  /// Every documented operation, in the specification's field order.
  pub fn operations_mut(&mut self) -> impl Iterator<Item = &mut Operation> {
    [
      &mut self.get,
      &mut self.put,
      &mut self.post,
      &mut self.delete,
      &mut self.options,
      &mut self.head,
      &mut self.patch,
      &mut self.trace,
    ]
    .into_iter()
    .filter_map(Option::as_mut)
  }
}

/// Where an [`SecurityScheme::ApiKey`] is sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApiKeyLocation {
  /// A query string key.
  Query,
  /// A request header.
  Header,
  /// A cookie (e.g. a session cookie).
  Cookie,
}

/// Security Scheme Object.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum SecurityScheme {
  /// HTTP authentication (`Authorization: <scheme> …`): bearer tokens, basic.
  #[serde(rename = "http", rename_all = "camelCase")]
  Http {
    /// Authorization scheme (`bearer`, `basic`).
    scheme: String,
    /// Bearer token format hint (`JWT`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bearer_format: Option<String>,
    /// Description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
  },
  /// A key in a header, query key or cookie (API keys, session cookies).
  #[serde(rename = "apiKey")]
  ApiKey {
    /// Header, query key or cookie name.
    name: String,
    /// Where the key is sent.
    #[serde(rename = "in")]
    location: ApiKeyLocation,
    /// Description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
  },
  /// OpenID Connect discovery.
  #[serde(rename = "openIdConnect", rename_all = "camelCase")]
  OpenIdConnect {
    /// Discovery document URL (`…/.well-known/openid-configuration`).
    open_id_connect_url: String,
    /// Description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
  },
}

impl SecurityScheme {
  /// `Authorization: Bearer <JWT>`.
  pub fn bearer_jwt() -> Self {
    Self::Http {
      scheme: "bearer".to_owned(),
      bearer_format: Some("JWT".to_owned()),
      description: None,
    }
  }

  /// `Authorization: Basic …`.
  pub fn basic() -> Self {
    Self::Http {
      scheme: "basic".to_owned(),
      bearer_format: None,
      description: None,
    }
  }

  /// A key named `name` sent in `location`.
  pub fn api_key(location: ApiKeyLocation, name: impl Into<String>) -> Self {
    Self::ApiKey {
      name: name.into(),
      location,
      description: None,
    }
  }

  /// A session cookie named `cookie`.
  pub fn cookie_session(cookie: impl Into<String>) -> Self {
    Self::api_key(ApiKeyLocation::Cookie, cookie)
  }

  /// OpenID Connect with its discovery document at `discovery_url`.
  pub fn openid_connect(discovery_url: impl Into<String>) -> Self {
    Self::OpenIdConnect {
      open_id_connect_url: discovery_url.into(),
      description: None,
    }
  }

  /// With a description.
  #[must_use]
  pub fn with_description(mut self, text: impl Into<String>) -> Self {
    let text = Some(text.into());
    match &mut self {
      Self::Http { description, .. }
      | Self::ApiKey { description, .. }
      | Self::OpenIdConnect { description, .. } => *description = text,
    }
    self
  }

  /// Whether a requirement on this scheme lists OAuth scopes (OpenID Connect) rather than role
  /// names.
  pub(crate) fn takes_scopes(&self) -> bool {
    matches!(self, Self::OpenIdConnect { .. })
  }
}

/// Components Object: the reusable schemas and security schemes.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Components {
  /// Schemas by component name.
  #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
  pub schemas: BTreeMap<String, Value>,
  /// Security schemes by name.
  #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
  pub security_schemes: BTreeMap<String, SecurityScheme>,
}

impl Components {
  /// No schemas and no security schemes.
  pub fn is_empty(&self) -> bool {
    self.schemas.is_empty() && self.security_schemes.is_empty()
  }
}
