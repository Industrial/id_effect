//! AI client errors.

use thiserror::Error;

/// Errors from LLM client operations.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum AiError {
  /// Empty prompt or message list.
  #[error("empty chat request")]
  EmptyRequest,
  /// Model returned no content.
  #[error("empty model response")]
  EmptyResponse,
  /// Transient upstream failure (retryable).
  #[error("transient upstream error (status {status})")]
  Transient {
    /// HTTP status code.
    status: u16,
    /// Redacted detail.
    detail: String,
  },
  /// Authentication failed.
  #[error("unauthorized")]
  Unauthorized,
  /// Upstream HTTP or vendor failure (detail redacted for logs).
  #[error("upstream error: {0}")]
  Upstream(String),
  /// JSON parse failure.
  #[error("invalid json: {0}")]
  InvalidJson(String),
  /// Cursor Agents API failure.
  #[error("cursor agents error: {0}")]
  CursorAgents(String),
}

impl AiError {
  /// Whether this error is safe to retry with backoff.
  #[inline]
  pub fn is_transient(&self) -> bool {
    matches!(self, Self::Transient { .. })
  }

  /// Map an HTTP status to an [`AiError`].
  pub fn from_http_status(status: u16, detail: impl Into<String>) -> Self {
    let detail = detail.into();
    match status {
      401 | 403 => Self::Unauthorized,
      429 | 502 | 503 | 504 => Self::Transient { status, detail },
      _ => Self::Upstream(format!("status {status}: {detail}")),
    }
  }
}
