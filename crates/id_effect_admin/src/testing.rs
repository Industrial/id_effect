//! Test helpers (feature `test-util`): an [`AdminRequest`] carries what the authentication and
//! tenancy layers would put on a real request — the principal, the tenant, htmx headers — and
//! [`AdminRequest::send`] answers it with an admin router, collecting an [`AdminResponse`].
//!
//! ```
//! use std::sync::Arc;
//!
//! use axum::http::StatusCode;
//! use id_effect_admin::testing::AdminRequest;
//! use id_effect_admin::{AdminDatabase, AdminSite};
//! use id_effect_authz::{BasicPrincipal, Guard, authenticated};
//! use id_effect_sql::{Dialect, TestSqlClient};
//!
//! # tokio::runtime::Runtime::new().unwrap().block_on(async {
//! let app: axum::Router = AdminSite::new()
//!   .guard(Guard::new("signed-in", authenticated()))
//!   .router::<()>(AdminDatabase::new(Arc::new(TestSqlClient::new()), Dialect::Sqlite))
//!   .into_axum();
//! let anonymous = AdminRequest::get("/admin").send(&app).await;
//! assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
//! let ada = AdminRequest::get("/admin").principal(BasicPrincipal::new("ada")).send(&app).await;
//! assert_eq!(ada.status, StatusCode::OK);
//! assert!(ada.body.contains("Nothing to administer."));
//! # });
//! ```

use axum::body::Body;
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, Request, StatusCode};
use id_effect_authz::{CurrentPrincipal, Principal};
use id_effect_axum::server::security::{CSRF_COOKIE, CSRF_FORM_FIELD};
use id_effect_sql::{CurrentTenant, ToSqlValue};
use tower::ServiceExt;

type Extend = Box<dyn FnOnce(&mut axum::http::Extensions) + Send>;

/// A request to an admin router (`GET`, or a form `POST`), with the request extensions real
/// layers would add.
#[must_use]
pub struct AdminRequest {
  method: Method,
  uri: String,
  body: String,
  headers: Vec<(HeaderName, HeaderValue)>,
  extensions: Vec<Extend>,
}

impl std::fmt::Debug for AdminRequest {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("AdminRequest")
      .field("method", &self.method)
      .field("uri", &self.uri)
      .field("headers", &self.headers)
      .finish_non_exhaustive()
  }
}

impl AdminRequest {
  /// `GET uri`, anonymous, without a tenant.
  pub fn get(uri: impl Into<String>) -> Self {
    Self {
      method: Method::GET,
      uri: uri.into(),
      body: String::new(),
      headers: Vec::new(),
      extensions: Vec::new(),
    }
  }

  /// `POST uri` with the `application/x-www-form-urlencoded` `body` (`title=Q3&status=draft`),
  /// anonymous, without a tenant.
  pub fn post(uri: impl Into<String>, body: impl Into<String>) -> Self {
    Self {
      method: Method::POST,
      body: body.into(),
      ..Self::get(uri)
    }
    .header(
      "content-type",
      HeaderValue::from_static("application/x-www-form-urlencoded"),
    )
  }

  /// With the CSRF `cookie` (`name=value`, [`AdminResponse::csrf_cookie`]) and the masked
  /// `token` in the `x-csrf-token` header ([`AdminResponse::csrf_token`]), as a browser session
  /// of an application behind `id_effect_axum`'s `SecurityLayer` sends them.
  ///
  /// # Panics
  ///
  /// When the cookie or the token is not a valid header value.
  pub fn csrf(self, cookie: &str, token: &str) -> Self {
    let value = |text: &str| {
      HeaderValue::from_str(text).unwrap_or_else(|e| panic!("admin test csrf header: {e}"))
    };
    self
      .header("cookie", value(cookie))
      .header("x-csrf-token", value(token))
  }

  /// Sent by `principal` (the `CurrentPrincipal` extension an authentication layer inserts).
  pub fn principal(self, principal: impl Principal) -> Self {
    self.extension(CurrentPrincipal::new(principal))
  }

  /// Within `tenant` (the `CurrentTenant` extension a tenancy layer inserts).
  pub fn tenant(self, tenant: impl ToSqlValue) -> Self {
    self.extension(CurrentTenant::new(tenant))
  }

  /// Any request extension.
  pub fn extension<T: Clone + Send + Sync + 'static>(mut self, value: T) -> Self {
    self.extensions.push(Box::new(move |ext| {
      ext.insert(value);
    }));
    self
  }

  /// An htmx request (`HX-Request: true`) targeting the element `target` (`HX-Target`).
  pub fn htmx(self, target: &'static str) -> Self {
    self
      .header("hx-request", HeaderValue::from_static("true"))
      .header("hx-target", HeaderValue::from_static(target))
  }

  /// A request header.
  pub fn header(mut self, name: &'static str, value: HeaderValue) -> Self {
    self.headers.push((HeaderName::from_static(name), value));
    self
  }

  /// The request.
  ///
  /// # Panics
  ///
  /// When the URI is not valid.
  pub fn into_request(self) -> Request<Body> {
    let mut req = Request::builder()
      .method(self.method)
      .uri(self.uri.as_str())
      .body(Body::from(self.body))
      .unwrap_or_else(|e| panic!("admin test request `{}`: {e}", self.uri));
    for (name, value) in self.headers {
      req.headers_mut().append(name, value);
    }
    for extend in self.extensions {
      extend(req.extensions_mut());
    }
    req
  }

  /// Answer the request with `app`.
  pub async fn send(self, app: &axum::Router) -> AdminResponse {
    let res = match app.clone().oneshot(self.into_request()).await {
      Ok(res) => res,
      Err(never) => match never {},
    };
    let status = res.status();
    let headers = res.headers().clone();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
      .await
      .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
      .unwrap_or_default();
    AdminResponse {
      status,
      headers,
      body,
    }
  }
}

/// What an admin router answered.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct AdminResponse {
  /// The status.
  pub status: StatusCode,
  /// The headers.
  pub headers: HeaderMap,
  /// The body as text.
  pub body: String,
}

impl AdminResponse {
  /// The masked CSRF token a page rendered into its forms (`<input name="csrf_token" …>`).
  pub fn csrf_token(&self) -> Option<String> {
    let marker = format!(r#"name="{CSRF_FORM_FIELD}" value=""#);
    let start = self.body.find(&marker)? + marker.len();
    let end = self.body[start..].find('"')?;
    Some(self.body[start..start + end].to_owned())
  }

  /// The `name=value` of the CSRF cookie the response set (`SecurityLayer` issues it on a
  /// session's first request).
  pub fn csrf_cookie(&self) -> Option<String> {
    self
      .headers
      .get_all("set-cookie")
      .iter()
      .filter_map(|v| v.to_str().ok())
      .find(|v| v.starts_with(CSRF_COOKIE))
      .and_then(|v| v.split(';').next())
      .map(str::to_owned)
  }

  /// The redirect target (`Location`).
  pub fn location(&self) -> Option<&str> {
    self.headers.get("location").and_then(|v| v.to_str().ok())
  }
}
