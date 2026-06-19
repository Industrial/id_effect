//! Shared HTTP helpers for vendor adapters.

use base64::{Engine as _, engine::general_purpose::STANDARD};
/// `Authorization: Bearer <token>`.
#[inline]
pub fn bearer_header(token: &str) -> (String, String) {
  ("Authorization".to_string(), format!("Bearer {token}"))
}

/// `Authorization: Basic` for Cursor (`api_key:` with empty password).
#[inline]
pub fn cursor_basic_auth_header(api_key: &str) -> (String, String) {
  let encoded = STANDARD.encode(format!("{api_key}:"));
  ("Authorization".to_string(), format!("Basic {encoded}"))
}

/// Join base URL and path without duplicate slashes.
pub fn join_url(base: &str, path: &str) -> String {
  let base = base.trim_end_matches('/');
  let path = path.trim_start_matches('/');
  format!("{base}/{path}")
}
