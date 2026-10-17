//! `GET /_console`: the dashboard, one static HTML page (inline CSS and vanilla JS, no build
//! step) that polls the JSON API under [`API_PREFIX`](crate::API_PREFIX).
//!
//! The page is served by [`Console::router`](crate::Console::router) behind the same guard as the
//! API, so a principal the guard denies never sees it. It reads the API with same-origin
//! credentials, renders every value with `textContent` (no HTML from the API reaches the DOM)
//! and loads nothing from another origin, which its `Content-Security-Policy` enforces.

use std::borrow::Cow;

use axum::extract::Request;
use axum::http::{HeaderValue, header};
use axum::response::{Html, IntoResponse, Response};
use id_effect_axum::server::security::CsrfToken;

/// Where the page is served.
pub(crate) const PATH: &str = "/_console";

/// The page.
const INDEX_HTML: &str = include_str!("index.html");

/// Inline script and style only; API calls to the same origin; no frames, forms or base URL
/// changes.
const CSP: &str = "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; \
                   connect-src 'self'; img-src 'self' data:; base-uri 'none'; form-action 'none'; \
                   frame-ancestors 'none'";

/// The page's CSRF meta tag, empty; filled with the session's token when the app runs behind
/// `id_effect_axum`'s `SecurityLayer`, whose CSRF cookie the page's script cannot read.
const CSRF_META: &str = r#"<meta name="csrf-token" content="">"#;

/// The page, with this session's [`CsrfToken`] (if a `SecurityLayer` with CSRF resolved one) in
/// its CSRF meta tag. The token is base64url, so it needs no HTML escaping.
fn page(token: Option<&CsrfToken>) -> Cow<'static, str> {
  match token {
    Some(token) => Cow::Owned(INDEX_HTML.replacen(
      CSRF_META,
      &format!(r#"<meta name="csrf-token" content="{}">"#, token.as_str()),
      1,
    )),
    None => Cow::Borrowed(INDEX_HTML),
  }
}

pub(crate) async fn index(request: Request) -> Response {
  let mut response = Html(page(request.extensions().get::<CsrfToken>())).into_response();
  let headers = response.headers_mut();
  headers.insert(
    header::CONTENT_SECURITY_POLICY,
    HeaderValue::from_static(CSP),
  );
  headers.insert(
    header::X_CONTENT_TYPE_OPTIONS,
    HeaderValue::from_static("nosniff"),
  );
  headers.insert(
    header::REFERRER_POLICY,
    HeaderValue::from_static("no-referrer"),
  );
  response
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn page_polls_every_api_route_and_loads_nothing_external() {
    for route in [
      "fibers",
      "supervisors",
      "metrics",
      "health",
      "log-level",
      "jobs",
    ] {
      assert!(
        INDEX_HTML.contains(&format!("'{route}'")),
        "the page does not read {route}"
      );
    }
    assert!(INDEX_HTML.contains("method: 'PUT'"));
    // The job buttons POST with the JSON content type the API requires of them.
    assert!(INDEX_HTML.contains("api('jobs/' + job.id + '/' + verb"));
    assert!(INDEX_HTML.contains("method: 'POST'"));
    // Every state-changing call sends the CSRF token from the meta tag the server fills.
    assert_eq!(INDEX_HTML.matches("headers: mutating()").count(), 2);
    assert!(INDEX_HTML.contains("headers['x-csrf-token'] = csrf"));
    assert_eq!(INDEX_HTML.matches(CSRF_META).count(), 1);
    for external in ["http://", "https://", "src=", "<link", "innerHTML"] {
      assert!(!INDEX_HTML.contains(external), "the page uses {external}");
    }
  }

  #[test]
  fn csp_allows_only_inline_code_and_same_origin_calls() {
    assert!(CSP.starts_with("default-src 'none';"));
    assert!(CSP.contains("connect-src 'self'"));
    assert!(CSP.contains("frame-ancestors 'none'"));
  }
}
