//! AI client errors.

use id_effect_platform::error::HttpError;
use thiserror::Error;

/// Upper bound on the upstream detail kept in an error (bytes, cut at a character boundary): a
/// vendor error body can be large and may echo request content.
pub(crate) const MAX_ERROR_DETAIL_BYTES: usize = 512;

/// Errors from LLM client operations.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum AiError {
  /// Empty prompt or message list.
  #[error("empty chat request")]
  EmptyRequest,
  /// A request the adapter refuses before sending it (for example `max_tokens` of 0).
  #[error("invalid request: {0}")]
  InvalidRequest(String),
  /// Model returned no content.
  #[error("empty model response")]
  EmptyResponse,
  /// Transient upstream failure (retryable).
  #[error("transient upstream error (status {status})")]
  Transient {
    /// HTTP status code.
    status: u16,
    /// Upstream detail, truncated.
    detail: String,
  },
  /// Authentication failed.
  #[error("unauthorized")]
  Unauthorized,
  /// Upstream HTTP or vendor failure (detail truncated).
  #[error("upstream error: {0}")]
  Upstream(String),
  /// JSON parse failure.
  #[error("invalid json: {0}")]
  InvalidJson(String),
  /// Cursor Agents API failure.
  #[error("cursor agents error: {0}")]
  CursorAgents(String),
  /// The platform client's SSRF policy refused the destination (ADR 0013 R6): a provider base
  /// URL on a loopback, private, link-local or metadata address, or a scheme other than
  /// `http`/`https`.
  #[error("request blocked: {0}")]
  Blocked(String),
  /// A finite limit was reached (ADR 0013 R4): `limit` names it (`response_bytes`,
  /// `stream_bytes`, `stream_events`, `sse_event_bytes`) and `max` is its configured value.
  #[error("limit exceeded: {limit} (max {max})")]
  LimitExceeded {
    /// Which limit.
    limit: &'static str,
    /// The configured maximum.
    max: u64,
  },
  /// The request did not complete within its timeout.
  #[error("request timed out")]
  Timeout,
}

impl AiError {
  /// Whether this error is safe to retry with backoff.
  #[inline]
  pub fn is_transient(&self) -> bool {
    matches!(self, Self::Transient { .. })
  }

  /// Map an HTTP status to an [`AiError`] (the detail is truncated).
  pub fn from_http_status(status: u16, detail: impl Into<String>) -> Self {
    let detail = truncate_detail(detail.into());
    match status {
      401 | 403 => Self::Unauthorized,
      // 529: Anthropic's "overloaded".
      429 | 502 | 503 | 504 | 529 => Self::Transient { status, detail },
      _ => Self::Upstream(format!("status {status}: {detail}")),
    }
  }
}

impl From<HttpError> for AiError {
  /// A transport failure: a body over its cap is [`AiError::LimitExceeded`], an SSRF refusal
  /// [`AiError::Blocked`], a timeout [`AiError::Timeout`], anything else
  /// [`AiError::Upstream`].
  fn from(e: HttpError) -> Self {
    match e {
      HttpError::BodyTooLarge { max, .. } => Self::LimitExceeded {
        limit: "response_bytes",
        max: max as u64,
      },
      HttpError::InvalidRequest(reason) if reason.contains("SSRF policy") => Self::Blocked(reason),
      HttpError::Reqwest(e) if e.is_timeout() => Self::Timeout,
      other => Self::Upstream(truncate_detail(format!("http: {other}"))),
    }
  }
}

/// `detail` cut to [`MAX_ERROR_DETAIL_BYTES`] at a character boundary.
pub(crate) fn truncate_detail(mut detail: String) -> String {
  if detail.len() > MAX_ERROR_DETAIL_BYTES {
    let mut cut = MAX_ERROR_DETAIL_BYTES;
    while !detail.is_char_boundary(cut) {
      cut -= 1;
    }
    detail.truncate(cut);
    detail.push('…');
  }
  detail
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn status_mapping() {
    assert_eq!(AiError::from_http_status(401, "x"), AiError::Unauthorized);
    assert_eq!(AiError::from_http_status(403, "x"), AiError::Unauthorized);
    assert!(AiError::from_http_status(429, "x").is_transient());
    assert!(AiError::from_http_status(503, "x").is_transient());
    assert_eq!(
      AiError::from_http_status(400, "bad"),
      AiError::Upstream("status 400: bad".into())
    );
  }

  #[test]
  fn long_details_are_truncated_on_a_char_boundary() {
    let long = "é".repeat(MAX_ERROR_DETAIL_BYTES);
    let cut = truncate_detail(long);
    assert!(cut.len() <= MAX_ERROR_DETAIL_BYTES + '…'.len_utf8());
    assert!(cut.ends_with('…'));
    assert_eq!(truncate_detail("short".into()), "short");
  }

  #[test]
  fn http_errors_map_to_typed_variants() {
    assert_eq!(
      AiError::from(HttpError::BodyTooLarge { len: 9, max: 8 }),
      AiError::LimitExceeded {
        limit: "response_bytes",
        max: 8
      }
    );
    assert!(matches!(
      AiError::from(HttpError::InvalidRequest(
        "blocked by the SSRF policy: loopback".into()
      )),
      AiError::Blocked(_)
    ));
    assert!(matches!(
      AiError::from(HttpError::InvalidRequest("bad header".into())),
      AiError::Upstream(_)
    ));
  }

  #[test]
  fn display_names_the_limit() {
    let e = AiError::LimitExceeded {
      limit: "stream_events",
      max: 3,
    };
    assert_eq!(e.to_string(), "limit exceeded: stream_events (max 3)");
    assert_eq!(AiError::Timeout.to_string(), "request timed out");
  }
}
