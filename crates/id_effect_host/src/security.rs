//! CSRF and CSP helpers for Axum application hosts.

use axum::extract::Request;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::sync::Arc;

/// Content-Security-Policy builder.
#[derive(Clone, Debug, Default)]
pub struct ContentSecurityPolicy {
  directives: Vec<String>,
}

impl ContentSecurityPolicy {
  /// Empty policy.
  pub fn new() -> Self {
    Self::default()
  }
  /// Add `default-src 'self'`.
  pub fn default_src_self(mut self) -> Self {
    self.directives.push("default-src 'self'".into());
    self
  }
  /// Add `script-src 'self'`.
  pub fn script_src_self(mut self) -> Self {
    self.directives.push("script-src 'self'".into());
    self
  }
  /// Serialize to CSP header value.
  pub fn to_header_value(&self) -> Option<HeaderValue> {
    if self.directives.is_empty() {
      return None;
    }
    HeaderValue::from_str(&self.directives.join("; ")).ok()
  }
}

/// Apply CSP on every response.
pub async fn csp_middleware(
  axum::extract::State(csp): axum::extract::State<Arc<ContentSecurityPolicy>>,
  request: Request,
  next: Next,
) -> Response {
  let mut response = next.run(request).await;
  if let Some(value) = csp.to_header_value() {
    response
      .headers_mut()
      .insert(header::CONTENT_SECURITY_POLICY, value);
  }
  response
}

/// CSRF token header name expected on mutating requests.
pub const CSRF_HEADER: &str = "x-csrf-token";

/// Shared CSRF secret for double-submit cookie pattern.
#[derive(Clone)]
pub struct CsrfConfig {
  token: Arc<str>,
}

impl CsrfConfig {
  /// Use `token` for validation on POST/PUT/PATCH/DELETE.
  pub fn new(token: impl Into<Arc<str>>) -> Self {
    Self {
      token: token.into(),
    }
  }
  /// Expected header value.
  pub fn token(&self) -> &str {
    &self.token
  }
}

/// Reject mutating requests when `X-CSRF-Token` does not match.
pub async fn csrf_middleware(
  axum::extract::State(config): axum::extract::State<CsrfConfig>,
  request: Request,
  next: Next,
) -> Response {
  let method = request.method().clone();
  if matches!(
    method,
    Method::POST | Method::PUT | Method::PATCH | Method::DELETE
  ) {
    let valid = request
      .headers()
      .get(CSRF_HEADER)
      .and_then(|v| v.to_str().ok())
      == Some(config.token());
    if !valid {
      return (StatusCode::FORBIDDEN, "csrf token mismatch").into_response();
    }
  }
  next.run(request).await
}

/// Set CSRF token response header.
pub fn set_csrf_header(headers: &mut HeaderMap, token: &str) {
  if let Ok(value) = HeaderValue::from_str(token) {
    headers.insert(CSRF_HEADER, value);
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use axum::Router;
  use axum::body::Body;
  use axum::routing::{delete, get, patch, post};
  use tower::ServiceExt;

  #[tokio::test]
  async fn csrf_middleware_blocks_post_without_token() {
    let config = CsrfConfig::new("secret");
    let app = Router::new().route("/", post(|| async { "ok" })).layer(
      axum::middleware::from_fn_with_state(config, csrf_middleware),
    );
    let res = app
      .oneshot(
        Request::builder()
          .method("POST")
          .uri("/")
          .body(Body::empty())
          .unwrap(),
      )
      .await
      .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
  }

  #[test]
  fn csp_empty_has_no_header_value() {
    assert!(ContentSecurityPolicy::new().to_header_value().is_none());
  }

  #[test]
  fn csrf_config_exposes_token() {
    assert_eq!(CsrfConfig::new("secret").token(), "secret");
  }

  #[test]
  fn set_csrf_header_inserts_value() {
    let mut headers = HeaderMap::new();
    set_csrf_header(&mut headers, "tok");
    assert_eq!(headers.get(CSRF_HEADER).unwrap(), "tok");
  }

  #[tokio::test]
  async fn csrf_allows_put_with_matching_token() {
    let config = CsrfConfig::new("secret");
    let app = Router::new()
      .route("/", post(|| async { "ok" }).put(|| async { "ok" }))
      .layer(axum::middleware::from_fn_with_state(
        config,
        csrf_middleware,
      ));
    let res = app
      .oneshot(
        Request::builder()
          .method("PUT")
          .uri("/")
          .header(CSRF_HEADER, "secret")
          .body(Body::empty())
          .unwrap(),
      )
      .await
      .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
  }

  #[test]
  fn csp_script_src_joins_directives() {
    let v = ContentSecurityPolicy::new()
      .default_src_self()
      .script_src_self()
      .to_header_value()
      .expect("csp");
    let s = v.to_str().unwrap();
    assert!(s.contains("default-src"));
    assert!(s.contains("script-src"));
  }

  #[tokio::test]
  async fn csrf_allows_get_without_token() {
    let config = CsrfConfig::new("secret");
    let app =
      Router::new()
        .route("/", get(|| async { "ok" }))
        .layer(axum::middleware::from_fn_with_state(
          config,
          csrf_middleware,
        ));
    let res = app
      .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
      .await
      .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
  }

  #[tokio::test]
  async fn csrf_rejects_delete_without_token() {
    let config = CsrfConfig::new("secret");
    let app = Router::new().route("/", delete(|| async { "ok" })).layer(
      axum::middleware::from_fn_with_state(config, csrf_middleware),
    );
    let res = app
      .oneshot(
        Request::builder()
          .method("DELETE")
          .uri("/")
          .body(Body::empty())
          .unwrap(),
      )
      .await
      .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
  }

  #[tokio::test]
  async fn csrf_rejects_patch_without_token() {
    let config = CsrfConfig::new("secret");
    let app = Router::new().route("/", patch(|| async { "ok" })).layer(
      axum::middleware::from_fn_with_state(config, csrf_middleware),
    );
    let res = app
      .oneshot(
        Request::builder()
          .method("PATCH")
          .uri("/")
          .body(Body::empty())
          .unwrap(),
      )
      .await
      .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
  }

  #[tokio::test]
  async fn csp_middleware_skips_header_when_empty() {
    let csp = Arc::new(ContentSecurityPolicy::new());
    let app = Router::new()
      .route("/", post(|| async { "ok" }))
      .layer(axum::middleware::from_fn_with_state(csp, csp_middleware));
    let res = app
      .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
      .await
      .unwrap();
    assert!(res.headers().get(header::CONTENT_SECURITY_POLICY).is_none());
  }
}
