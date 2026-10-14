//! API version negotiation for RPC-shaped Axum routes.

use axum::extract::Request;
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::sync::Arc;

/// Parsed API version label (e.g. `"v1"`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ApiVersion(pub String);

impl ApiVersion {
  /// Construct a version label.
  #[inline]
  pub fn new(label: impl Into<String>) -> Self {
    Self(label.into())
  }
}

/// Configuration for version routing middleware.
#[derive(Clone, Debug)]
pub struct VersionConfig {
  /// Default version when the client omits a preference.
  pub default: ApiVersion,
  /// Supported version labels (must include `default`).
  pub supported: Arc<[ApiVersion]>,
}

impl VersionConfig {
  /// Build config ensuring `default` is listed in `supported`.
  pub fn new(default: ApiVersion, supported: Vec<ApiVersion>) -> Self {
    let mut supported = supported;
    if !supported.iter().any(|v| v == &default) {
      supported.push(default.clone());
    }
    Self {
      default,
      supported: supported.into(),
    }
  }

  fn resolve(&self, headers: &HeaderMap, path: &str) -> Option<ApiVersion> {
    if let Some(v) = extract_header_version(headers) {
      if self.supported.iter().any(|s| s == &v) {
        return Some(v);
      }
      return None;
    }
    if let Some(v) = extract_path_version(path) {
      if self.supported.iter().any(|s| s == &v) {
        return Some(v);
      }
      return None;
    }
    Some(self.default.clone())
  }
}

/// Header name for explicit version negotiation.
pub const ACCEPT_VERSION: &str = "accept-version";

/// Response header echoing the negotiated version.
pub const API_VERSION_HEADER: &str = "api-version";

/// Extract version from `Accept-Version` header.
pub fn extract_header_version(headers: &HeaderMap) -> Option<ApiVersion> {
  headers
    .get(ACCEPT_VERSION)
    .and_then(|v| v.to_str().ok())
    .map(|s| ApiVersion(s.trim().to_string()))
    .filter(|v| !v.0.is_empty())
}

/// Extract version from path prefix `/vN/`.
pub fn extract_path_version(path: &str) -> Option<ApiVersion> {
  let rest = path.strip_prefix('/')?;
  let segment = rest.split('/').next()?;
  if segment.starts_with('v')
    && segment.len() > 1
    && segment[1..].chars().all(|c| c.is_ascii_digit())
  {
    Some(ApiVersion(segment.to_string()))
  } else {
    None
  }
}

/// Insert `api-version` on successful responses.
pub fn set_response_version(headers: &mut HeaderMap, version: &ApiVersion) {
  if let Ok(value) = version.0.parse() {
    headers.insert(API_VERSION_HEADER, value);
  }
}

/// Middleware: attach resolved [`ApiVersion`] as request extension; 406 when unsupported.
pub async fn negotiate_api_version(
  axum::extract::State(config): axum::extract::State<VersionConfig>,
  mut request: Request,
  next: Next,
) -> Response {
  let path = request.uri().path().to_string();
  match config.resolve(request.headers(), &path) {
    Some(version) => {
      request.extensions_mut().insert(version);
      next.run(request).await
    }
    None => (
      StatusCode::NOT_ACCEPTABLE,
      format!("unsupported API version; supported: {:?}", config.supported),
    )
      .into_response(),
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use axum::Router;
  use axum::body::Body;
  use axum::http::HeaderMap;
  use axum::routing::get;
  use tower::ServiceExt;

  #[test]
  fn extract_path_version_parses_v_prefix() {
    assert_eq!(
      extract_path_version("/v2/users"),
      Some(ApiVersion("v2".into()))
    );
    assert_eq!(extract_path_version("/health"), None);
  }

  #[test]
  fn extract_header_version_reads_accept_version() {
    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT_VERSION, "v2".parse().unwrap());
    assert_eq!(
      extract_header_version(&headers),
      Some(ApiVersion("v2".into()))
    );
  }

  #[test]
  fn version_config_includes_default_in_supported() {
    let cfg = VersionConfig::new(ApiVersion("v1".into()), vec![ApiVersion("v2".into())]);
    assert!(cfg.supported.iter().any(|v| v.0 == "v1"));
    assert!(cfg.supported.iter().any(|v| v.0 == "v2"));
  }

  #[test]
  fn set_response_version_inserts_header() {
    let mut headers = HeaderMap::new();
    set_response_version(&mut headers, &ApiVersion("v3".into()));
    assert_eq!(headers.get(API_VERSION_HEADER).unwrap(), "v3");
  }

  #[tokio::test]
  async fn middleware_returns_406_for_unsupported_header_version() {
    let config = VersionConfig::new(ApiVersion("v1".into()), vec![ApiVersion("v1".into())]);
    let app =
      Router::new()
        .route("/", get(|| async { "ok" }))
        .layer(axum::middleware::from_fn_with_state(
          config,
          negotiate_api_version,
        ));
    let res = app
      .oneshot(
        Request::builder()
          .uri("/")
          .header(ACCEPT_VERSION, "v9")
          .body(Body::empty())
          .unwrap(),
      )
      .await
      .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_ACCEPTABLE);
  }

  #[tokio::test]
  async fn middleware_uses_path_version_when_present() {
    let config = VersionConfig::new(
      ApiVersion("v1".into()),
      vec![ApiVersion("v1".into()), ApiVersion("v2".into())],
    );
    let app = Router::new()
      .route(
        "/v2/hello",
        get(|ext: axum::Extension<ApiVersion>| async move { ext.0.0.clone() }),
      )
      .layer(axum::middleware::from_fn_with_state(
        config,
        negotiate_api_version,
      ));
    let res = app
      .oneshot(
        Request::builder()
          .uri("/v2/hello")
          .body(Body::empty())
          .unwrap(),
      )
      .await
      .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
  }

  #[tokio::test]
  async fn middleware_defaults_when_unspecified() {
    let config = VersionConfig::new(ApiVersion("v1".into()), vec![ApiVersion("v1".into())]);
    let app = Router::new()
      .route(
        "/",
        get(|ext: axum::Extension<ApiVersion>| async move { ext.0.0.clone() }),
      )
      .layer(axum::middleware::from_fn_with_state(
        config,
        negotiate_api_version,
      ));

    let res = app
      .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
      .await
      .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
  }
}
