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

/// The `next` runtime this crate's v1 code bridges `id_effect_platform` HTTP effects onto
/// (`next::compat::IntoV1::into_v1_on`): the ambient tokio runtime, whose IO driver reqwest
/// needs, or [`Runtime::global`](id_effect::next::Runtime::global) outside one. Removed when this
/// crate switches to `next` (l-labs-edge).
pub(crate) fn next_http_runtime() -> id_effect::next::Runtime {
  match id_effect::next::TokioExecutor::current() {
    Some(exec) => id_effect::next::Runtime::from_executor(std::sync::Arc::new(exec)),
    None => id_effect::next::Runtime::global().clone(),
  }
}

/// Join base URL and path without duplicate slashes.
pub fn join_url(base: &str, path: &str) -> String {
  let base = base.trim_end_matches('/');
  let path = path.trim_start_matches('/');
  format!("{base}/{path}")
}
