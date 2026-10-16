//! [`ErrorBody`]: the JSON body of every error response.

use serde::Serialize;

/// `{"error": "<code>", "message": "<text>"}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ErrorBody {
  /// A stable machine-readable code (`not_found`, `conflict`, `invalid`, `unavailable`,
  /// `timeout`, `internal`).
  pub error: &'static str,
  /// A human-readable message.
  pub message: String,
}
