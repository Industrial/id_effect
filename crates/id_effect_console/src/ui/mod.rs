//! `GET /_console`: the dashboard, one static HTML page (inline CSS and vanilla JS, no build
//! step) that polls the JSON API under [`API_PREFIX`](crate::API_PREFIX).
//!
//! The page is served by [`Console::router`](crate::Console::router) behind the same guard as the
//! API, so a principal the guard denies never sees it. It reads the API with same-origin
//! credentials, renders every value with `textContent` (no HTML from the API reaches the DOM)
//! and loads nothing from another origin, which its `Content-Security-Policy` enforces.

use axum::http::{HeaderValue, header};
use axum::response::{Html, IntoResponse, Response};

/// Where the page is served.
pub(crate) const PATH: &str = "/_console";

/// The page.
const INDEX_HTML: &str = include_str!("index.html");

/// Inline script and style only; API calls to the same origin; no frames, forms or base URL
/// changes.
const CSP: &str = "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; \
                   connect-src 'self'; img-src 'self' data:; base-uri 'none'; form-action 'none'; \
                   frame-ancestors 'none'";

pub(crate) async fn index() -> Response {
  let mut response = Html(INDEX_HTML).into_response();
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
