//! Shared HTTP helpers for vendor adapters.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use id_effect::{Effect, fail, succeed};

/// `Authorization: Bearer <token>` header pair.
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

/// Lifts a `Result` into a ready [`Effect`] (success or typed failure).
#[inline]
pub(crate) fn from_result<A, E>(r: Result<A, E>) -> Effect<A, E> {
  match r {
    Ok(a) => succeed(a),
    Err(e) => fail(e),
  }
}

/// Maps a vendor response status to `Unauthorized` (401/403) or
/// [`AiError::from_http_status`](crate::AiError::from_http_status) (any other non-2xx); `detail`
/// is computed only on failure.
#[cfg(any(
  feature = "openai",
  feature = "anthropic",
  feature = "ollama",
  feature = "cursor"
))]
pub(crate) fn check_status(
  status: u16,
  detail: impl FnOnce() -> String,
) -> Result<(), crate::error::AiError> {
  use crate::error::AiError;
  if status == 401 || status == 403 {
    return Err(AiError::Unauthorized);
  }
  if !(200..300).contains(&status) {
    return Err(AiError::from_http_status(status, detail()));
  }
  Ok(())
}

/// Join base URL and path without duplicate slashes.
pub fn join_url(base: &str, path: &str) -> String {
  let base = base.trim_end_matches('/');
  let path = path.trim_start_matches('/');
  format!("{base}/{path}")
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn headers_and_urls() {
    assert_eq!(bearer_header("t").1, format!("{} t", "Bearer"));
    assert_eq!(cursor_basic_auth_header("k").1, "Basic azo=");
    assert_eq!(join_url("https://a/", "/v1/x"), "https://a/v1/x");
    assert!(
      from_result::<u8, ()>(Err(()))
        .block_on()
        .into_result()
        .is_err()
    );
  }

  #[cfg(feature = "openai")]
  #[test]
  fn check_status_maps_non_2xx() {
    use crate::error::AiError;
    assert_eq!(check_status(200, String::new), Ok(()));
    assert_eq!(check_status(401, String::new), Err(AiError::Unauthorized));
    assert!(check_status(500, || "boom".into()).is_err());
  }
}
