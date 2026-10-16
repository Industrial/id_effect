//! CORS: [`CorsConfig`] builds a [`tower_http::cors::CorsLayer`], rejecting unsafe combinations
//! as a typed [`SecurityError`] instead of tower-http's runtime panic.

use std::time::Duration;

use axum::http::{HeaderName, HeaderValue, Method, Uri};
use tower_http::cors::{AllowOrigin, Any, CorsLayer};

use super::SecurityError;

/// Cross-origin resource sharing policy. Nothing is allowed until configured.
///
/// ```rust
/// use axum::http::Method;
/// use id_effect_axum::server::security::CorsConfig;
///
/// let cors = CorsConfig::new()
///   .allow_origin("https://app.example")
///   .allow_methods([Method::GET, Method::POST])
///   .allow_credentials(true)
///   .build()
///   .expect("valid CORS config");
/// # let _ = cors;
/// assert!(CorsConfig::new().allow_any_origin().allow_credentials(true).build().is_err());
/// ```
#[derive(Clone, Debug, Default)]
pub struct CorsConfig {
  origins: Vec<String>,
  any_origin: bool,
  methods: Vec<Method>,
  headers: Vec<HeaderName>,
  expose: Vec<HeaderName>,
  credentials: bool,
  max_age: Option<Duration>,
}

impl CorsConfig {
  /// A policy that allows nothing.
  pub fn new() -> Self {
    Self::default()
  }

  /// Allow `origin` (`scheme://host[:port]`, no path). `"*"` is the same as
  /// [`allow_any_origin`](Self::allow_any_origin).
  pub fn allow_origin(mut self, origin: impl Into<String>) -> Self {
    self.origins.push(origin.into());
    self
  }

  /// Allow every origin (`Access-Control-Allow-Origin: *`; incompatible with credentials).
  pub fn allow_any_origin(mut self) -> Self {
    self.any_origin = true;
    self
  }

  /// Methods allowed in preflight responses.
  pub fn allow_methods(mut self, methods: impl IntoIterator<Item = Method>) -> Self {
    self.methods.extend(methods);
    self
  }

  /// Request headers allowed in preflight responses.
  pub fn allow_headers(mut self, headers: impl IntoIterator<Item = HeaderName>) -> Self {
    self.headers.extend(headers);
    self
  }

  /// Response headers exposed to the calling script.
  pub fn expose_headers(mut self, headers: impl IntoIterator<Item = HeaderName>) -> Self {
    self.expose.extend(headers);
    self
  }

  /// Send `Access-Control-Allow-Credentials: true` (cookies and auth headers).
  pub fn allow_credentials(mut self, allow: bool) -> Self {
    self.credentials = allow;
    self
  }

  /// How long browsers may cache a preflight response.
  pub fn max_age(mut self, max_age: Duration) -> Self {
    self.max_age = Some(max_age);
    self
  }

  /// The tower-http layer, or [`SecurityError::CorsCredentialsWithWildcard`] /
  /// [`SecurityError::InvalidCorsOrigin`].
  pub fn build(&self) -> Result<CorsLayer, SecurityError> {
    let wildcard = self.any_origin || self.origins.iter().any(|o| o == "*");
    if wildcard && self.credentials {
      return Err(SecurityError::CorsCredentialsWithWildcard);
    }
    let origin = if wildcard {
      AllowOrigin::from(Any)
    } else {
      let list = self
        .origins
        .iter()
        .map(|o| parse_origin(o))
        .collect::<Result<Vec<_>, _>>()?;
      AllowOrigin::list(list)
    };
    let mut layer = CorsLayer::new()
      .allow_origin(origin)
      .allow_methods(self.methods.clone())
      .allow_headers(self.headers.clone())
      .expose_headers(self.expose.clone())
      .allow_credentials(self.credentials);
    if let Some(max_age) = self.max_age {
      layer = layer.max_age(max_age);
    }
    Ok(layer)
  }
}

fn parse_origin(origin: &str) -> Result<HeaderValue, SecurityError> {
  let invalid = || SecurityError::InvalidCorsOrigin {
    origin: origin.to_owned(),
  };
  let uri: Uri = origin.parse().map_err(|_| invalid())?;
  // `Uri` reports an empty path as "/", so a trailing slash is checked on the text.
  let bare = uri.scheme().is_some()
    && uri.authority().is_some()
    && uri.path() == "/"
    && uri.query().is_none()
    && !origin.ends_with('/');
  if !bare {
    return Err(invalid());
  }
  HeaderValue::from_str(origin).map_err(|_| invalid())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn cors_origin_must_be_bare_scheme_host() {
    assert!(parse_origin("https://app.example").is_ok());
    assert!(parse_origin("http://localhost:3000").is_ok());
    for bad in [
      "app.example",
      "https://app.example/",
      "https://a.example/x",
      "",
    ] {
      assert_eq!(
        parse_origin(bad),
        Err(SecurityError::InvalidCorsOrigin {
          origin: bad.to_owned()
        }),
        "{bad}"
      );
    }
  }

  #[test]
  fn cors_star_origin_counts_as_wildcard() {
    let err = CorsConfig::new()
      .allow_origin("*")
      .allow_credentials(true)
      .build()
      .unwrap_err();
    assert_eq!(err, SecurityError::CorsCredentialsWithWildcard);
    assert!(CorsConfig::new().allow_origin("*").build().is_ok());
  }
}
