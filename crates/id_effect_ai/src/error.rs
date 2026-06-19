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
  /// Upstream HTTP or vendor failure (detail redacted for logs).
  #[error("upstream error: {0}")]
  Upstream(String),
  /// JSON parse failure.
  #[error("invalid json: {0}")]
  InvalidJson(String),
}
