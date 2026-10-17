//! The in-process HTTP test client (feature `http`): requests sent straight to a tower
//! [`Service`] — an axum `Router`, the app's whole router with its middleware — without a
//! socket, a port or a server task.
//!
//! [`TestClient::new(router)`](TestClient::new) wraps the service; `get` / `post_json` / … build a
//! [`TestRequest`]; [`TestRequest::send`] is an [`Effect`] returning a [`TestResponse`] with the
//! status, headers and the whole body, read back as text, JSON ([`TestResponse::json`]) or an
//! RFC 9457 problem ([`TestResponse::problem`]).
//!
//! ```rust
//! use axum::Json;
//! use axum::routing::{get, post};
//! use id_effect::Exit;
//! use id_effect_test::http::{StatusCode, TestClient};
//! use serde_json::{Value, json};
//!
//! let app = axum::Router::new()
//!   .route("/health", get(|| async { "ok" }))
//!   .route("/echo", post(|Json(v): Json<Value>| async move { Json(v) }));
//! let client = TestClient::new(app);
//!
//! let Exit::Success(health) = client.get("/health").send().block_on() else { panic!() };
//! assert_eq!((health.status(), health.text()), (StatusCode::OK, "ok".to_owned()));
//!
//! let Exit::Success(echo) = client.post_json("/echo", &json!({"n": 1})).send().block_on() else {
//!   panic!()
//! };
//! assert_eq!(echo.json::<Value>().unwrap(), json!({"n": 1}));
//! ```

use std::collections::BTreeMap;
use std::fmt;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use id_effect::{Effect, from_async};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tower::{Service, ServiceExt};

pub use ::http::{
  HeaderMap, HeaderName, HeaderValue, Method, Request, Response, StatusCode, Uri, header,
};

/// What the client sends: the whole request body in memory.
pub type TestBody = Full<Bytes>;

/// A request that could not be built or sent, or a response body that could not be read or
/// decoded.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum HttpTestError {
  /// The request is invalid (URI, header name or value, body serialization).
  Request(String),
  /// The service failed (returned its error instead of a response).
  Service(String),
  /// The response body could not be read or decoded (JSON, problem).
  Body(String),
}

impl fmt::Display for HttpTestError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Request(m) => write!(f, "invalid test request: {m}"),
      Self::Service(m) => write!(f, "the service failed: {m}"),
      Self::Body(m) => write!(f, "response body: {m}"),
    }
  }
}

impl std::error::Error for HttpTestError {}

/// An in-process HTTP client over a tower [`Service`]: each request is a `oneshot` call on a
/// clone of the service. Cloning the client is cheap when the service is (an axum `Router` is).
#[derive(Clone)]
pub struct TestClient<S> {
  service: S,
  headers: HeaderMap,
}

impl<S> fmt::Debug for TestClient<S> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("TestClient")
      .field("service", &std::any::type_name::<S>())
      .field("headers", &self.headers)
      .finish()
  }
}

impl<S> TestClient<S> {
  /// A client sending to `service`.
  pub fn new(service: S) -> Self {
    Self {
      service,
      headers: HeaderMap::new(),
    }
  }

  /// Sends `name: value` with every request (an API key, a tenant); a request's own header of
  /// the same name replaces it.
  ///
  /// # Panics
  ///
  /// When `name` or `value` is not a valid header name or value.
  #[must_use]
  #[track_caller]
  pub fn default_header(mut self, name: &str, value: &str) -> Self {
    let name = HeaderName::try_from(name).expect("TestClient::default_header: invalid name");
    let value = HeaderValue::try_from(value).expect("TestClient::default_header: invalid value");
    self.headers.insert(name, value);
    self
  }

  /// The service.
  pub fn service(&self) -> &S {
    &self.service
  }
}

impl<S: Clone> TestClient<S> {
  /// A `method` request to `uri` (a path, `/accounts?limit=10`, or an absolute URI).
  pub fn request(&self, method: Method, uri: &str) -> TestRequest<S> {
    TestRequest {
      service: self.service.clone(),
      method,
      uri: uri.to_owned(),
      headers: self.headers.clone(),
      body: Bytes::new(),
      error: None,
    }
  }

  /// `GET uri`.
  pub fn get(&self, uri: &str) -> TestRequest<S> {
    self.request(Method::GET, uri)
  }

  /// `POST uri` (add a body with [`TestRequest::json`] / [`TestRequest::body`]).
  pub fn post(&self, uri: &str) -> TestRequest<S> {
    self.request(Method::POST, uri)
  }

  /// `PUT uri`.
  pub fn put(&self, uri: &str) -> TestRequest<S> {
    self.request(Method::PUT, uri)
  }

  /// `PATCH uri`.
  pub fn patch(&self, uri: &str) -> TestRequest<S> {
    self.request(Method::PATCH, uri)
  }

  /// `DELETE uri`.
  pub fn delete(&self, uri: &str) -> TestRequest<S> {
    self.request(Method::DELETE, uri)
  }

  /// `POST uri` with `body` as JSON.
  pub fn post_json<T: Serialize + ?Sized>(&self, uri: &str, body: &T) -> TestRequest<S> {
    self.post(uri).json(body)
  }

  /// `PUT uri` with `body` as JSON.
  pub fn put_json<T: Serialize + ?Sized>(&self, uri: &str, body: &T) -> TestRequest<S> {
    self.put(uri).json(body)
  }

  /// `PATCH uri` with `body` as JSON.
  pub fn patch_json<T: Serialize + ?Sized>(&self, uri: &str, body: &T) -> TestRequest<S> {
    self.patch(uri).json(body)
  }
}

/// A request being built; [`send`](Self::send) it. Invalid input (a bad header, a body that does
/// not serialize) is reported by `send` as [`HttpTestError::Request`].
pub struct TestRequest<S> {
  service: S,
  method: Method,
  uri: String,
  headers: HeaderMap,
  body: Bytes,
  error: Option<HttpTestError>,
}

impl<S> fmt::Debug for TestRequest<S> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("TestRequest")
      .field("method", &self.method)
      .field("uri", &self.uri)
      .field("headers", &self.headers)
      .field("body_len", &self.body.len())
      .finish()
  }
}

impl<S> TestRequest<S> {
  fn invalid(mut self, message: String) -> Self {
    self.error.get_or_insert(HttpTestError::Request(message));
    self
  }

  /// Sets header `name` to `value` (replacing a default header of that name).
  #[must_use]
  pub fn header(mut self, name: &str, value: &str) -> Self {
    match (HeaderName::try_from(name), HeaderValue::try_from(value)) {
      (Ok(name), Ok(value)) => {
        self.headers.insert(name, value);
        self
      }
      (Err(e), _) => self.invalid(format!("header name `{name}`: {e}")),
      (_, Err(e)) => self.invalid(format!("header `{name}` value: {e}")),
    }
  }

  /// `Authorization: Bearer <token>`.
  #[must_use]
  pub fn bearer(self, token: &str) -> Self {
    self.header(header::AUTHORIZATION.as_str(), &format!("Bearer {token}"))
  }

  /// `body` serialized as JSON, with `Content-Type: application/json`.
  #[must_use]
  pub fn json<T: Serialize + ?Sized>(self, body: &T) -> Self {
    match serde_json::to_vec(body) {
      Ok(bytes) => self.body(bytes, "application/json"),
      Err(e) => self.invalid(format!("JSON body: {e}")),
    }
  }

  /// `body` as is, with `Content-Type: content_type`.
  #[must_use]
  pub fn body(mut self, body: impl Into<Bytes>, content_type: &str) -> Self {
    self.body = body.into();
    self.header(header::CONTENT_TYPE.as_str(), content_type)
  }

  /// The request as `http` builds it.
  fn into_parts(self) -> Result<(S, Request<TestBody>), HttpTestError> {
    if let Some(e) = self.error {
      return Err(e);
    }
    let mut request = Request::builder()
      .method(self.method)
      .uri(self.uri.as_str())
      .body(Full::new(self.body))
      .map_err(|e| HttpTestError::Request(format!("`{}`: {e}", self.uri)))?;
    *request.headers_mut() = self.headers;
    Ok((self.service, request))
  }
}

impl<S, B> TestRequest<S>
where
  S: Service<Request<TestBody>, Response = Response<B>> + Send + 'static,
  S::Error: fmt::Display + Send,
  S::Future: Send,
  B: http_body::Body + Send + 'static,
  B::Data: Send,
  B::Error: fmt::Display,
{
  /// Sends the request through a clone of the service and reads the whole response. The effect
  /// is runnable on any executor the service runs on (`block_on` for a plain router; a tokio
  /// runtime when handlers need one).
  pub fn send(self) -> Effect<TestResponse, HttpTestError> {
    from_async(move || async move {
      let (service, request) = self.into_parts()?;
      let (method, uri) = (request.method().clone(), request.uri().clone());
      let response = service
        .oneshot(request)
        .await
        .map_err(|e| HttpTestError::Service(e.to_string()))?;
      let (parts, body) = response.into_parts();
      let body = body
        .collect()
        .await
        .map_err(|e| HttpTestError::Body(e.to_string()))?
        .to_bytes();
      Ok(TestResponse {
        method,
        uri,
        status: parts.status,
        headers: parts.headers,
        body,
      })
    })
  }
}

/// A response read whole: status, headers and body, and the method and URI of the request that
/// got it.
#[derive(Clone, Debug)]
pub struct TestResponse {
  method: Method,
  uri: Uri,
  status: StatusCode,
  headers: HeaderMap,
  body: Bytes,
}

impl TestResponse {
  /// The method of the request.
  pub fn method(&self) -> &Method {
    &self.method
  }

  /// The URI of the request (as sent: a path, with its query if any).
  pub fn uri(&self) -> &Uri {
    &self.uri
  }

  /// The status.
  pub fn status(&self) -> StatusCode {
    self.status
  }

  /// The headers.
  pub fn headers(&self) -> &HeaderMap {
    &self.headers
  }

  /// Header `name` as text, if present and visible ASCII.
  pub fn header(&self, name: &str) -> Option<&str> {
    self.headers.get(name).and_then(|v| v.to_str().ok())
  }

  /// The body.
  pub fn bytes(&self) -> &Bytes {
    &self.body
  }

  /// The body as text (invalid UTF-8 replaced).
  pub fn text(&self) -> String {
    String::from_utf8_lossy(&self.body).into_owned()
  }

  /// The body decoded as JSON.
  pub fn json<T: DeserializeOwned>(&self) -> Result<T, HttpTestError> {
    serde_json::from_slice(&self.body)
      .map_err(|e| HttpTestError::Body(format!("not the expected JSON ({e}): {}", self.text())))
  }

  /// The body as an RFC 9457 problem; an error unless the content type is
  /// `application/problem+json`.
  pub fn problem(&self) -> Result<Problem, HttpTestError> {
    let content_type = self.header(header::CONTENT_TYPE.as_str()).unwrap_or("");
    if !content_type.starts_with("application/problem+json") {
      return Err(HttpTestError::Body(format!(
        "not a problem: {} with content type `{content_type}`: {}",
        self.status,
        self.text()
      )));
    }
    self.json()
  }

  /// Panics, showing the body, unless the status is `expected`; returns the response otherwise.
  #[track_caller]
  pub fn assert_status(&self, expected: StatusCode) -> &Self {
    assert_eq!(
      self.status,
      expected,
      "unexpected status; body: {}",
      self.text()
    );
    self
  }
}

/// An RFC 9457 `application/problem+json` body.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Problem {
  /// The problem type URI (`about:blank` when absent).
  #[serde(rename = "type", default = "about_blank")]
  pub type_uri: String,
  /// A short summary of the problem type.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub title: Option<String>,
  /// The HTTP status.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub status: Option<u16>,
  /// This occurrence, in words.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub detail: Option<String>,
  /// This occurrence, as a URI.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub instance: Option<String>,
  /// Every other member (`errors`, …).
  #[serde(flatten)]
  pub extensions: BTreeMap<String, serde_json::Value>,
}

fn about_blank() -> String {
  "about:blank".to_owned()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn errors_name_their_stage() {
    assert!(
      HttpTestError::Request("x".into())
        .to_string()
        .starts_with("invalid test request")
    );
    assert!(
      HttpTestError::Service("x".into())
        .to_string()
        .starts_with("the service failed")
    );
    assert!(
      HttpTestError::Body("x".into())
        .to_string()
        .starts_with("response body")
    );
  }

  #[test]
  fn a_problem_defaults_its_type() {
    let p: Problem = serde_json::from_str(r#"{"title":"Bad","errors":[1]}"#).unwrap();
    assert_eq!(p.type_uri, "about:blank");
    assert_eq!(p.extensions["errors"], serde_json::json!([1]));
  }
}
