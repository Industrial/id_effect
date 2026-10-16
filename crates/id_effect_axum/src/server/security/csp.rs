//! Content-Security-Policy: a directive builder, per-request nonces ([`CspNonce`]) and script
//! hashes.

use std::fmt;
use std::sync::Arc;

use axum::extract::{FromRequestParts, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::Response;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use sha2::{Digest, Sha256};

use super::SecurityError;

/// Bytes of randomness in a [`CspNonce`].
const NONCE_BYTES: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Source {
  Literal(String),
  Nonce,
}

/// Content-Security-Policy builder.
///
/// Sources are written as they appear in the header (`'self'`, `'none'`,
/// `https://cdn.example`). [`script_nonce`](Self::script_nonce) adds a per-request
/// `'nonce-…'` placeholder filled by [`SecurityLayer`](super::SecurityLayer), which exposes the
/// value to handlers as the [`CspNonce`] extractor.
#[derive(Clone, Debug, Default)]
pub struct ContentSecurityPolicy {
  directives: Vec<(String, Vec<Source>)>,
}

macro_rules! fetch_directives {
  ($($method:ident => $name:literal),* $(,)?) => {
    $(
      #[doc = concat!("Add `sources` to `", $name, "`.")]
      pub fn $method(self, sources: &[&str]) -> Self {
        self.directive($name, sources)
      }
    )*
  };
}

impl ContentSecurityPolicy {
  /// Empty policy.
  pub fn new() -> Self {
    Self::default()
  }

  /// `default-src 'self'; script-src 'self' 'nonce-…'; object-src 'none'; base-uri 'self';
  /// frame-ancestors 'none'; form-action 'self'`.
  pub fn recommended() -> Self {
    Self::new()
      .default_src(&["'self'"])
      .script_src(&["'self'"])
      .script_nonce()
      .object_src(&["'none'"])
      .base_uri(&["'self'"])
      .frame_ancestors(&["'none'"])
      .form_action(&["'self'"])
  }

  /// Add `sources` to the directive `name` (created on first use; duplicates are dropped).
  pub fn directive(mut self, name: &str, sources: &[&str]) -> Self {
    self.push(
      name,
      sources.iter().map(|s| Source::Literal((*s).to_owned())),
    );
    self
  }

  fetch_directives! {
    default_src => "default-src",
    script_src => "script-src",
    style_src => "style-src",
    img_src => "img-src",
    connect_src => "connect-src",
    font_src => "font-src",
    object_src => "object-src",
    media_src => "media-src",
    frame_src => "frame-src",
    worker_src => "worker-src",
    manifest_src => "manifest-src",
    child_src => "child-src",
    frame_ancestors => "frame-ancestors",
    base_uri => "base-uri",
    form_action => "form-action",
    report_uri => "report-uri",
    report_to => "report-to",
  }

  /// Add `default-src 'self'`.
  pub fn default_src_self(self) -> Self {
    self.default_src(&["'self'"])
  }

  /// Add `script-src 'self'`.
  pub fn script_src_self(self) -> Self {
    self.script_src(&["'self'"])
  }

  /// Add the valueless `upgrade-insecure-requests` directive.
  pub fn upgrade_insecure_requests(mut self) -> Self {
    self.push("upgrade-insecure-requests", []);
    self
  }

  /// Allow inline/external scripts carrying this request's nonce (`script-src 'nonce-…'`).
  pub fn script_nonce(mut self) -> Self {
    self.push("script-src", [Source::Nonce]);
    self
  }

  /// Allow inline styles carrying this request's nonce (`style-src 'nonce-…'`).
  pub fn style_nonce(mut self) -> Self {
    self.push("style-src", [Source::Nonce]);
    self
  }

  /// Allow exactly the inline script `content` (`script-src 'sha256-…'`).
  pub fn script_hash(self, content: &str) -> Self {
    let source = sha256_source(content);
    self.script_src(&[source.as_str()])
  }

  /// Allow exactly the inline style `content` (`style-src 'sha256-…'`).
  pub fn style_hash(self, content: &str) -> Self {
    let source = sha256_source(content);
    self.style_src(&[source.as_str()])
  }

  /// Whether a directive uses the per-request nonce.
  pub fn uses_nonce(&self) -> bool {
    self
      .directives
      .iter()
      .any(|(_, sources)| sources.contains(&Source::Nonce))
  }

  /// Check every directive name and source; [`SecurityLayer::csp`](super::SecurityLayer::csp)
  /// calls this so a bad policy is a typed error, never a silently missing header.
  pub fn validate(&self) -> Result<(), SecurityError> {
    for (name, sources) in &self.directives {
      let name_ok =
        !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
      let sources_ok = sources.iter().all(|source| match source {
        Source::Literal(s) => {
          !s.is_empty()
            && s
              .bytes()
              .all(|b| b.is_ascii_graphic() && b != b';' && b != b',')
        }
        Source::Nonce => true,
      });
      if !name_ok || !sources_ok {
        return Err(SecurityError::InvalidCspDirective {
          directive: name.clone(),
        });
      }
    }
    Ok(())
  }

  /// Serialize to a header value without a nonce (`None` when empty or invalid).
  pub fn to_header_value(&self) -> Option<HeaderValue> {
    self.header_value(None)
  }

  /// Serialize to a header value with `nonce` in every nonce placeholder.
  pub fn header_value_with_nonce(&self, nonce: &CspNonce) -> Option<HeaderValue> {
    self.header_value(Some(nonce.as_str()))
  }

  fn header_value(&self, nonce: Option<&str>) -> Option<HeaderValue> {
    if self.directives.is_empty() || self.validate().is_err() {
      return None;
    }
    let rendered: Vec<String> = self
      .directives
      .iter()
      .map(|(name, sources)| {
        let mut out = name.clone();
        for source in sources {
          match (source, nonce) {
            (Source::Literal(s), _) => {
              out.push(' ');
              out.push_str(s);
            }
            (Source::Nonce, Some(n)) => {
              out.push_str(" 'nonce-");
              out.push_str(n);
              out.push('\'');
            }
            (Source::Nonce, None) => {}
          }
        }
        out
      })
      .collect();
    HeaderValue::from_str(&rendered.join("; ")).ok()
  }

  fn push(&mut self, name: &str, sources: impl IntoIterator<Item = Source>) {
    let index = match self.directives.iter().position(|(n, _)| n == name) {
      Some(index) => index,
      None => {
        self.directives.push((name.to_owned(), Vec::new()));
        self.directives.len() - 1
      }
    };
    let list = &mut self.directives[index].1;
    for source in sources {
      if !list.contains(&source) {
        list.push(source);
      }
    }
  }
}

fn sha256_source(content: &str) -> String {
  format!(
    "'sha256-{}'",
    STANDARD.encode(Sha256::digest(content.as_bytes()))
  )
}

/// This request's CSP nonce: put it in `<script nonce="…">`. Extract it in a handler behind a
/// [`SecurityLayer`](super::SecurityLayer) whose policy uses a nonce.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CspNonce(Arc<str>);

impl CspNonce {
  /// A fresh nonce (128 random bits, base64url).
  pub fn generate() -> Self {
    Self(super::random_token(NONCE_BYTES).into())
  }

  /// The nonce text.
  pub fn as_str(&self) -> &str {
    &self.0
  }
}

impl fmt::Display for CspNonce {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(&self.0)
  }
}

impl<S> FromRequestParts<S> for CspNonce
where
  S: Send + Sync,
{
  type Rejection = (StatusCode, &'static str);

  async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
    parts.extensions.get::<CspNonce>().cloned().ok_or((
      StatusCode::INTERNAL_SERVER_ERROR,
      "no CSP nonce: SecurityLayer with a nonce policy is not installed",
    ))
  }
}

/// Header value for this request; when the policy uses a nonce, a fresh [`CspNonce`] is
/// generated and stored in the request extensions.
pub(crate) fn prepare(
  policy: &ContentSecurityPolicy,
  request: &mut Request,
) -> Option<HeaderValue> {
  if !policy.uses_nonce() {
    return policy.to_header_value();
  }
  let nonce = CspNonce::generate();
  let value = policy.header_value_with_nonce(&nonce);
  request.extensions_mut().insert(nonce);
  value
}

/// Apply CSP on every response (overwriting a handler's header).
#[deprecated(
  since = "0.8.0",
  note = "use `SecurityLayer::recommended(key)` (or `.csp(policy)`), which adds nonces and keeps handler overrides"
)]
pub async fn csp_middleware(
  State(csp): State<Arc<ContentSecurityPolicy>>,
  mut request: Request,
  next: Next,
) -> Response {
  let value = prepare(&csp, &mut request);
  let mut response = next.run(request).await;
  if let Some(value) = value {
    response
      .headers_mut()
      .insert(header::CONTENT_SECURITY_POLICY, value);
  }
  response
}

#[cfg(test)]
#[allow(deprecated)]
mod tests {
  use super::*;
  use axum::Router;
  use axum::body::Body;
  use axum::routing::{get, post};
  use tower::ServiceExt;

  #[test]
  fn csp_empty_has_no_header_value() {
    assert!(ContentSecurityPolicy::new().to_header_value().is_none());
  }

  #[test]
  fn csp_script_src_joins_directives() {
    let v = ContentSecurityPolicy::new()
      .default_src_self()
      .script_src_self()
      .to_header_value()
      .expect("csp");
    assert_eq!(v, "default-src 'self'; script-src 'self'");
  }

  #[test]
  fn csp_directives_merge_and_dedupe() {
    let v = ContentSecurityPolicy::new()
      .img_src(&["'self'"])
      .img_src(&["'self'", "data:"])
      .upgrade_insecure_requests()
      .to_header_value()
      .unwrap();
    assert_eq!(v, "img-src 'self' data:; upgrade-insecure-requests");
  }

  #[test]
  fn csp_nonce_rendered_only_with_nonce() {
    let policy = ContentSecurityPolicy::recommended();
    assert!(policy.uses_nonce());
    let nonce = CspNonce::generate();
    let with = policy.header_value_with_nonce(&nonce).unwrap();
    assert!(with.to_str().unwrap().contains(&format!("'nonce-{nonce}'")));
    assert!(
      !policy
        .to_header_value()
        .unwrap()
        .to_str()
        .unwrap()
        .contains("nonce")
    );
  }

  #[test]
  fn csp_script_hash_is_sha256_base64() {
    // sha256("") in base64.
    let v = ContentSecurityPolicy::new()
      .script_hash("")
      .to_header_value()
      .unwrap();
    assert_eq!(
      v,
      "script-src 'sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU='"
    );
  }

  #[test]
  fn csp_validate_rejects_injection() {
    let bad = ContentSecurityPolicy::new().directive("img-src", &["* ; script-src *"]);
    assert!(bad.validate().is_err());
    assert!(bad.to_header_value().is_none());
    assert!(
      ContentSecurityPolicy::new()
        .directive("", &["'self'"])
        .validate()
        .is_err()
    );
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

  #[tokio::test]
  async fn csp_middleware_shim_fills_nonce() {
    let csp = Arc::new(ContentSecurityPolicy::recommended());
    let app = Router::new()
      .route("/", get(|nonce: CspNonce| async move { nonce.to_string() }))
      .layer(axum::middleware::from_fn_with_state(csp, csp_middleware));
    let res = app
      .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
      .await
      .unwrap();
    let header = res.headers()[header::CONTENT_SECURITY_POLICY]
      .to_str()
      .unwrap()
      .to_owned();
    let body = axum::body::to_bytes(res.into_body(), 1024).await.unwrap();
    let nonce = std::str::from_utf8(&body).unwrap();
    assert!(header.contains(&format!("'nonce-{nonce}'")));
  }
}
