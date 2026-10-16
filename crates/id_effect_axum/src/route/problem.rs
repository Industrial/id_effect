//! RFC 9457 *Problem Details for HTTP APIs*: the [`Problem`] error body
//! (`Content-Type: application/problem+json`) and the [`Issue`]s of a rejected request.
//!
//! Typed routes and the [`extract`](super::extract) extractors answer input that does not decode
//! with a [`Problem`] whose `errors` member lists every [`Issue`] at once:
//!
//! ```json
//! {
//!   "type": "about:blank",
//!   "title": "Unprocessable Entity",
//!   "status": 422,
//!   "detail": "the request input failed validation (2 issues)",
//!   "instance": "/accounts",
//!   "errors": [
//!     { "in": "query", "pointer": "/limit", "message": "expected i64" },
//!     { "in": "body", "pointer": "/age", "message": "expected i64" }
//!   ]
//! }
//! ```
//!
//! `pointer` is a JSON Pointer (RFC 6901) into the request part named by `in` (`""` = the part
//! itself). A body that is not well-formed (JSON syntax error) answers **400**; well-formed input
//! that fails its schema answers **422**. [`Problem`] is reusable for any other error response.

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use id_effect::schema::ParseError;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

/// Media type of a [`Problem`] body.
pub const PROBLEM_JSON: &str = "application/problem+json";

/// The default problem `type` (RFC 9457 §4.2.1): the status code says it all.
pub const ABOUT_BLANK: &str = "about:blank";

/// Where in the request an [`Issue`] was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Location {
  /// A path parameter.
  Path,
  /// The query string.
  Query,
  /// A request header.
  Header,
  /// The request body (JSON or form).
  Body,
}

/// One failure of a request part, as listed in [`Problem::errors`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Issue {
  /// Request part that failed.
  #[serde(rename = "in")]
  pub location: Location,
  /// JSON Pointer (RFC 6901) to the failing value within that part (`""` = the part itself).
  pub pointer: String,
  /// Human-readable reason.
  pub message: String,
}

impl Issue {
  /// Issue at `pointer` in `location`.
  pub fn new(location: Location, pointer: impl Into<String>, message: impl Into<String>) -> Self {
    Self {
      location,
      pointer: pointer.into(),
      message: message.into(),
    }
  }

  /// Issue for a schema [`ParseError`], its dot-separated path turned into a JSON Pointer
  /// (`items.0.name` → `/items/0/name`).
  pub fn from_parse_error(location: Location, error: &ParseError) -> Self {
    Self::new(location, json_pointer(&error.path), error.message.clone())
  }
}

/// JSON Pointer for a dot-separated schema path; `~` and `/` in a segment are escaped
/// (`~0`, `~1`).
pub(crate) fn json_pointer(path: &str) -> String {
  if path.is_empty() {
    return String::new();
  }
  path.split('.').fold(String::new(), |mut out, segment| {
    out.push('/');
    out.push_str(&segment.replace('~', "~0").replace('/', "~1"));
    out
  })
}

fn serialize_status<S: Serializer>(status: &StatusCode, s: S) -> Result<S::Ok, S::Error> {
  s.serialize_u16(status.as_u16())
}

fn deserialize_status<'de, D: Deserializer<'de>>(d: D) -> Result<StatusCode, D::Error> {
  let code = u16::deserialize(d)?;
  StatusCode::from_u16(code).map_err(serde::de::Error::custom)
}

fn about_blank() -> String {
  ABOUT_BLANK.to_owned()
}

/// An RFC 9457 problem details object; responds with its [`status`](Problem::status) and
/// `Content-Type: application/problem+json`.
///
/// ```rust
/// use axum::http::StatusCode;
/// use id_effect_axum::route::{Issue, Location, Problem};
///
/// let problem = Problem::new(StatusCode::CONFLICT)
///   .with_detail("account name already taken")
///   .with_instance("/accounts")
///   .with_error(Issue::new(Location::Body, "/name", "taken"))
///   .with_extension("retry", serde_json::json!(false));
/// let body = serde_json::to_value(&problem).unwrap();
/// assert_eq!(body["title"], "Conflict");
/// assert_eq!(body["status"], 409);
/// assert_eq!(body["errors"][0]["pointer"], "/name");
/// assert_eq!(body["retry"], false);
/// ```
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Problem {
  /// URI reference identifying the problem type ([`ABOUT_BLANK`] by default).
  #[serde(rename = "type", default = "about_blank")]
  pub type_uri: String,
  /// Short, human-readable summary of the problem type.
  #[serde(default)]
  pub title: String,
  /// HTTP status of this occurrence (serialized as a number).
  #[serde(
    serialize_with = "serialize_status",
    deserialize_with = "deserialize_status"
  )]
  pub status: StatusCode,
  /// Explanation specific to this occurrence.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub detail: Option<String>,
  /// URI reference identifying this occurrence (the typed extractors use the request path).
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub instance: Option<String>,
  /// Every input issue (extension member; omitted when empty).
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub errors: Vec<Issue>,
  /// Further extension members, serialized at the top level.
  #[serde(flatten)]
  pub extensions: Map<String, Value>,
}

impl Problem {
  /// `about:blank` problem for `status`, titled with its reason phrase.
  pub fn new(status: StatusCode) -> Self {
    Self {
      type_uri: about_blank(),
      title: status.canonical_reason().unwrap_or_default().to_owned(),
      status,
      detail: None,
      instance: None,
      errors: Vec::new(),
      extensions: Map::new(),
    }
  }

  /// Rejected request input: **400** when the request is malformed, otherwise **422**
  /// (`status` is used as given), with every issue and a detail counting them.
  pub fn invalid_input(status: StatusCode, errors: Vec<Issue>) -> Self {
    let what = if status == StatusCode::BAD_REQUEST {
      "the request is malformed"
    } else {
      "the request input failed validation"
    };
    let count = match errors.len() {
      1 => "1 issue".to_owned(),
      n => format!("{n} issues"),
    };
    Self::new(status)
      .with_detail(format!("{what} ({count})"))
      .with_errors(errors)
  }

  /// Set the problem `type` URI.
  pub fn with_type(mut self, type_uri: impl Into<String>) -> Self {
    self.type_uri = type_uri.into();
    self
  }

  /// Set the `title`.
  pub fn with_title(mut self, title: impl Into<String>) -> Self {
    self.title = title.into();
    self
  }

  /// Set the `detail`.
  pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
    self.detail = Some(detail.into());
    self
  }

  /// Set the `instance`.
  pub fn with_instance(mut self, instance: impl Into<String>) -> Self {
    self.instance = Some(instance.into());
    self
  }

  /// Append one [`Issue`].
  pub fn with_error(mut self, issue: Issue) -> Self {
    self.errors.push(issue);
    self
  }

  /// Append several [`Issue`]s.
  pub fn with_errors(mut self, issues: impl IntoIterator<Item = Issue>) -> Self {
    self.errors.extend(issues);
    self
  }

  /// Add an extension member; `key` must not be a standard member name (`type`, `title`,
  /// `status`, `detail`, `instance`, `errors`).
  pub fn with_extension(mut self, key: impl Into<String>, value: Value) -> Self {
    self.extensions.insert(key.into(), value);
    self
  }
}

impl IntoResponse for Problem {
  fn into_response(self) -> Response {
    // Strings, numbers and `serde_json::Value`s always serialize.
    let body = serde_json::to_vec(&self).unwrap_or_default();
    (
      self.status,
      [(header::CONTENT_TYPE, HeaderValue::from_static(PROBLEM_JSON))],
      body,
    )
      .into_response()
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use http_body_util::BodyExt;
  use serde_json::json;

  #[test]
  fn pointers_escape_and_index() {
    assert_eq!(json_pointer(""), "");
    assert_eq!(json_pointer("items.0.name"), "/items/0/name");
    assert_eq!(json_pointer("a~b.c/d"), "/a~0b/c~1d");
    let issue = Issue::from_parse_error(Location::Query, &ParseError::new("limit", "bad"));
    assert_eq!(issue, Issue::new(Location::Query, "/limit", "bad"));
  }

  #[test]
  fn invalid_input_counts_issues() {
    let one = Problem::invalid_input(
      StatusCode::UNPROCESSABLE_ENTITY,
      vec![Issue::new(Location::Path, "", "x")],
    );
    assert_eq!(
      one.detail.as_deref(),
      Some("the request input failed validation (1 issue)")
    );
    assert_eq!(
      Problem::new(StatusCode::UNPROCESSABLE_ENTITY)
        .with_error(Issue::new(Location::Path, "", "x"))
        .errors,
      one.errors
    );
    let two = Problem::invalid_input(StatusCode::BAD_REQUEST, Vec::new());
    assert_eq!(
      two.detail.as_deref(),
      Some("the request is malformed (0 issues)")
    );
    assert_eq!(two.title, "Bad Request");
  }

  #[tokio::test]
  async fn responds_as_problem_json_and_round_trips() {
    let problem = Problem::new(StatusCode::NOT_FOUND)
      .with_type("https://example.com/probs/missing")
      .with_title("Missing")
      .with_extension("id", json!(7));
    let res = problem.clone().into_response();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    assert_eq!(res.headers()[header::CONTENT_TYPE], PROBLEM_JSON);
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
      v,
      json!({"type": "https://example.com/probs/missing", "title": "Missing", "status": 404, "id": 7})
    );
    let back: Problem = serde_json::from_value(v).unwrap();
    assert_eq!(back, problem);

    let minimal: Problem = serde_json::from_value(json!({"status": 418})).unwrap();
    assert_eq!(minimal.type_uri, ABOUT_BLANK);
    assert!(serde_json::from_value::<Problem>(json!({"status": 1000})).is_err());
  }
}
