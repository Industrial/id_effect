//! Web security for Axum application hosts: CSRF, CORS, CSP (with per-request nonces), default
//! security headers and cookie defaults with signed/private jars.
//!
//! [`SecurityLayer::recommended`] bundles the secure defaults into one tower layer:
//!
//! - CSRF on: a random per-session secret in a signed `__Host-` cookie, rendered as a token
//!   masked per response and required on every state-changing request (see [`csrf`]);
//! - a nonce-based [`ContentSecurityPolicy::recommended`] policy (see [`csp`]);
//! - [`SecurityHeaders::recommended`] (HSTS, `X-Frame-Options: DENY`, `nosniff`,
//!   `Referrer-Policy`, COOP, CORP, `Permissions-Policy`), each set only when the handler did
//!   not set it;
//! - [`CookieDefaults::recommended`] and the [`CookieKey`] for the [`Cookies`] extractor.
//!
//! CORS is a separate layer built from [`CorsConfig`]. The [`CookieKey`] comes from an effect
//! [`Layer`](id_effect::Layer): [`CookieKey::layer`] reads it from config.
//!
//! ```rust
//! use axum::{Router, routing::post};
//! use id_effect_axum::server::security::{CookieKey, SecurityLayer};
//!
//! let app: Router = Router::new()
//!   .route("/notes", post(|| async { "saved" }))
//!   .route("/webhooks/stripe", post(|| async { "ok" }))
//!   .layer(SecurityLayer::recommended(CookieKey::generate()).csrf_exempt("/webhooks"));
//! # let _ = app;
//! ```

use std::convert::Infallible;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::extract::Request;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore;
use tower_layer::Layer;
use tower_service::Service;

pub mod cookies;
pub mod cors;
pub mod csp;
pub mod csrf;
pub mod headers;

pub use cookies::{COOKIE_KEY_ENV, CookieDefaults, CookieKey, Cookies};
pub use cors::CorsConfig;
#[allow(deprecated)]
pub use csp::csp_middleware;
pub use csp::{ContentSecurityPolicy, CspNonce};
pub use csrf::{CSRF_COOKIE, CSRF_FORM_FIELD, CSRF_HEADER, CsrfConfig, CsrfToken};
#[allow(deprecated)]
pub use csrf::{csrf_middleware, set_csrf_header};
pub use headers::{FrameOptions, SecurityHeaders};

use csp::CompiledCsp;
use csrf::CsrfProtection;

/// A security configuration error, reported when the configuration is built (never a panic).
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SecurityError {
  /// Cookie key material shorter than [`cookies::COOKIE_KEY_MIN_LEN`] bytes.
  CookieKeyTooShort {
    /// Bytes supplied.
    len: usize,
  },
  /// Cookie key text is not valid base64.
  CookieKeyEncoding,
  /// A signed or private cookie was requested but no [`CookieKey`] is installed.
  MissingCookieKey,
  /// CORS `allow_credentials(true)` combined with a wildcard origin.
  CorsCredentialsWithWildcard,
  /// A CORS origin that is not `scheme://host[:port]`.
  InvalidCorsOrigin {
    /// The rejected origin.
    origin: String,
  },
  /// A CSP directive name or source that would break the header (empty, `;`, `,`, whitespace
  /// inside a source, or a non-visible character).
  InvalidCspDirective {
    /// The offending directive.
    directive: String,
  },
  /// A header value that is not a valid HTTP header value.
  InvalidHeaderValue {
    /// Header name.
    header: &'static str,
    /// The rejected value.
    value: String,
  },
}

impl fmt::Display for SecurityError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::CookieKeyTooShort { len } => write!(
        f,
        "cookie key too short: expected at least {} bytes, got {len}",
        cookies::COOKIE_KEY_MIN_LEN
      ),
      Self::CookieKeyEncoding => f.write_str("cookie key is not valid base64"),
      Self::MissingCookieKey => f.write_str("no cookie key installed (add SecurityLayer)"),
      Self::CorsCredentialsWithWildcard => {
        f.write_str("CORS allow-credentials cannot be combined with a wildcard origin")
      }
      Self::InvalidCorsOrigin { origin } => {
        write!(
          f,
          "invalid CORS origin {origin:?}: expected scheme://host[:port]"
        )
      }
      Self::InvalidCspDirective { directive } => {
        write!(f, "invalid Content-Security-Policy directive {directive:?}")
      }
      Self::InvalidHeaderValue { header, value } => {
        write!(f, "invalid value {value:?} for header {header}")
      }
    }
  }
}

impl std::error::Error for SecurityError {}

/// `bytes` bytes from the thread-local CSPRNG, base64url-encoded without padding.
pub(crate) fn random_token(bytes: usize) -> String {
  let mut buf = vec![0u8; bytes];
  rand::rng().fill_bytes(&mut buf);
  URL_SAFE_NO_PAD.encode(buf)
}

/// The most headers [`SecurityLayer`] adds to a response: the CSRF cookie, CSP and the seven
/// [`SecurityHeaders`].
const SECURITY_RESPONSE_HEADERS: usize = 9;

/// Secure-by-default tower layer: CSRF, CSP with nonce, security headers and cookie defaults.
///
/// Built with [`SecurityLayer::recommended`]; every part can be replaced or switched off
/// explicitly. Apply it with `Router::layer`.
#[derive(Clone, Debug)]
pub struct SecurityLayer {
  key: CookieKey,
  csrf: Option<CsrfProtection>,
  csp: Option<CompiledCsp>,
  headers: Option<SecurityHeaders>,
  cookie_defaults: CookieDefaults,
}

impl SecurityLayer {
  /// All protections on: CSRF on every unsafe method, [`ContentSecurityPolicy::recommended`],
  /// [`SecurityHeaders::recommended`] and [`CookieDefaults::recommended`]. `key` signs the CSRF
  /// cookie and backs the [`Cookies`] signed/private jars.
  pub fn recommended(key: CookieKey) -> Self {
    Self {
      csrf: Some(CsrfProtection::new(key.clone())),
      key,
      csp: Some(CompiledCsp::new(&ContentSecurityPolicy::recommended())),
      headers: Some(SecurityHeaders::recommended()),
      cookie_defaults: CookieDefaults::recommended(),
    }
  }

  /// Skip the CSRF check for paths under `prefix` (whole segments: `/webhooks` matches
  /// `/webhooks/stripe` but not `/webhooksx`). The token is still issued.
  pub fn csrf_exempt(mut self, prefix: impl Into<String>) -> Self {
    if let Some(csrf) = &mut self.csrf {
      csrf.exempt(prefix.into());
    }
    self
  }

  /// Skip CSRF for bearer-only requests: an `Authorization: Bearer …` header and no `Cookie`
  /// header at all. Nothing a browser attaches by itself authenticates such a request, so a
  /// cross-site page cannot forge it; an app serving both browser sessions and API tokens keeps
  /// one stack. A request that carries any cookie (a session, or the CSRF cookie) is still
  /// checked, and so are `Basic` and other schemes (browsers resend those cross-site). A skipped
  /// request gets no CSRF token or cookie (the [`CsrfToken`] extractor rejects it).
  pub fn csrf_exempt_bearer(mut self) -> Self {
    if let Some(csrf) = &mut self.csrf {
      csrf.exempt_bearer();
    }
    self
  }

  /// Use `config`'s form-body limit and read timeout for the CSRF check (default
  /// [`CsrfConfig::recommended`]: 64 KiB, 10 s).
  pub fn csrf_config(mut self, config: CsrfConfig) -> Self {
    if let Some(csrf) = &mut self.csrf {
      csrf.configure(&config);
    }
    self
  }

  /// Switch CSRF protection off entirely (e.g. a pure bearer-token API with no cookies).
  pub fn without_csrf(mut self) -> Self {
    self.csrf = None;
    self
  }

  /// Use `policy` instead of the recommended CSP; it is validated here.
  pub fn csp(mut self, policy: ContentSecurityPolicy) -> Result<Self, SecurityError> {
    policy.validate()?;
    self.csp = Some(CompiledCsp::new(&policy));
    Ok(self)
  }

  /// Send no Content-Security-Policy header.
  pub fn without_csp(mut self) -> Self {
    self.csp = None;
    self
  }

  /// Use `headers` instead of [`SecurityHeaders::recommended`].
  pub fn headers(mut self, headers: SecurityHeaders) -> Self {
    self.headers = Some(headers);
    self
  }

  /// Send none of the default security headers.
  pub fn without_security_headers(mut self) -> Self {
    self.headers = None;
    self
  }

  /// Use `defaults` for cookies added through the [`Cookies`] extractor.
  pub fn cookie_defaults(mut self, defaults: CookieDefaults) -> Self {
    self.cookie_defaults = defaults;
    self
  }

  async fn handle<S>(self: Arc<Self>, mut inner: S, mut request: Request) -> Response
  where
    S: Service<Request, Error = Infallible>,
    S::Response: IntoResponse,
  {
    request.extensions_mut().insert(self.key.clone());
    request
      .extensions_mut()
      .insert(self.cookie_defaults.clone());
    let csp = self
      .csp
      .as_ref()
      .and_then(|policy| policy.prepare(&mut request));
    let (outcome, cookie_update) = match &self.csrf {
      Some(csrf) => {
        let (outcome, update) = csrf.guard(request).await;
        (outcome, Some(update))
      }
      None => (Ok(request), None),
    };
    let mut response = match outcome {
      Ok(request) => match inner.call(request).await {
        Ok(response) => response.into_response(),
        Err(never) => match never {},
      },
      Err(rejection) => rejection,
    };
    // Resolved after the handler ran: it may have rotated the token.
    let set_cookie = self
      .csrf
      .as_ref()
      .zip(cookie_update)
      .and_then(|(csrf, update)| csrf.cookie_header(update));
    let headers = response.headers_mut();
    // Room for what follows (cookie, CSP, the default headers): the map grows at most once.
    headers.reserve(SECURITY_RESPONSE_HEADERS);
    if let Some(cookie) = set_cookie {
      headers.append(header::SET_COOKIE, cookie);
    }
    if let Some(value) = csp {
      headers
        .entry(header::CONTENT_SECURITY_POLICY)
        .or_insert(value);
    }
    if let Some(defaults) = &self.headers {
      defaults.apply(headers);
    }
    response
  }
}

impl<S> Layer<S> for SecurityLayer {
  type Service = SecurityService<S>;

  fn layer(&self, inner: S) -> Self::Service {
    SecurityService {
      inner,
      config: Arc::new(self.clone()),
    }
  }
}

/// The [`Service`] produced by [`SecurityLayer`].
#[derive(Clone, Debug)]
pub struct SecurityService<S> {
  inner: S,
  config: Arc<SecurityLayer>,
}

impl<S> Service<Request> for SecurityService<S>
where
  S: Service<Request, Error = Infallible> + Clone + Send + 'static,
  S::Response: IntoResponse,
  S::Future: Send + 'static,
{
  type Response = Response;
  type Error = Infallible;
  type Future = Pin<Box<dyn Future<Output = Result<Response, Infallible>> + Send>>;

  fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
    self.inner.poll_ready(cx)
  }

  fn call(&mut self, request: Request) -> Self::Future {
    // The ready service handles this request; the clone waits for the next `poll_ready`.
    let clone = self.inner.clone();
    let inner = std::mem::replace(&mut self.inner, clone);
    let config = Arc::clone(&self.config);
    Box::pin(async move { Ok(config.handle(inner, request).await) })
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn random_token_is_url_safe_and_sized() {
    let token = random_token(32);
    assert_eq!(URL_SAFE_NO_PAD.decode(&token).unwrap().len(), 32);
    assert!(
      token
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    );
    assert_ne!(token, random_token(32));
  }

  #[test]
  fn security_error_display_names_the_problem() {
    let e = SecurityError::CookieKeyTooShort { len: 3 };
    assert!(e.to_string().contains("got 3"));
    assert!(
      SecurityError::CorsCredentialsWithWildcard
        .to_string()
        .contains("wildcard")
    );
  }

  #[test]
  fn csp_override_is_validated() {
    let layer = SecurityLayer::recommended(CookieKey::generate());
    let bad = ContentSecurityPolicy::new().directive("script-src", &["'self'; img-src *"]);
    assert!(matches!(
      layer.csp(bad),
      Err(SecurityError::InvalidCspDirective { .. })
    ));
  }

  #[test]
  fn without_csrf_drops_exemptions() {
    let layer = SecurityLayer::recommended(CookieKey::generate())
      .without_csrf()
      .csrf_exempt("/hooks");
    assert!(layer.csrf.is_none());
  }
}
