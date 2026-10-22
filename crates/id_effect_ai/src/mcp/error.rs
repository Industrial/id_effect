//! [`McpError`]: why an MCP client call or a transport failed.

use id_effect_platform::error::HttpError;
use thiserror::Error;

use super::message::RpcError;
use crate::error::truncate_detail;

/// Why an MCP call or transport failed.
#[derive(Debug, Clone, PartialEq, Error)]
#[non_exhaustive]
pub enum McpError {
  /// The server answered the request with a JSON-RPC error.
  #[error("mcp server error: {0}")]
  Rpc(RpcError),
  /// The platform client's SSRF policy refused the server's address (ADR 0013 R6); nothing
  /// was sent.
  #[error("mcp destination blocked: {0}")]
  Blocked(String),
  /// The connection failed or closed (I/O, HTTP transport, the peer went away).
  #[error("mcp transport: {0}")]
  Transport(String),
  /// The server answered with an HTTP status and no JSON-RPC error.
  #[error("mcp http status {status}: {detail}")]
  Http {
    /// The status.
    status: u16,
    /// The body (truncated).
    detail: String,
  },
  /// The peer sent something that is not a valid MCP message or answer.
  #[error("mcp protocol: {0}")]
  Protocol(String),
  /// A header given to the client is not allowed: an invalid name or value, or one the
  /// protocol owns (`content-type`, `accept`, `mcp-*`, ...).
  #[error("mcp client header refused: {0}")]
  InvalidHeader(String),
  /// A message exceeded the cap (ADR 0013 R4).
  #[error("mcp message exceeds {max} bytes")]
  TooLarge {
    /// The cap in bytes.
    max: usize,
  },
}

impl McpError {
  /// The JSON-RPC error the server answered with, if that is what this is.
  pub fn rpc(&self) -> Option<&RpcError> {
    match self {
      Self::Rpc(e) => Some(e),
      _ => None,
    }
  }

  pub(crate) fn transport(detail: impl std::fmt::Display) -> Self {
    Self::Transport(truncate_detail(detail.to_string()))
  }
}

impl From<HttpError> for McpError {
  /// An SSRF refusal is [`McpError::Blocked`], a body over its cap [`McpError::TooLarge`],
  /// anything else [`McpError::Transport`].
  fn from(e: HttpError) -> Self {
    match e {
      HttpError::InvalidRequest(reason) if reason.contains("SSRF policy") => Self::Blocked(reason),
      HttpError::BodyTooLarge { max, .. } => Self::TooLarge { max },
      other => Self::transport(format!("http: {other}")),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn http_errors_map_to_mcp_errors() {
    assert!(matches!(
      McpError::from(HttpError::InvalidRequest("refused by SSRF policy".into())),
      McpError::Blocked(_)
    ));
    assert_eq!(
      McpError::from(HttpError::BodyTooLarge { len: 9, max: 4 }),
      McpError::TooLarge { max: 4 }
    );
    let other = McpError::from(HttpError::InvalidRequest("bad header".into()));
    assert!(matches!(other, McpError::Transport(_)));
    assert!(other.rpc().is_none());
    let rpc = McpError::Rpc(RpcError::internal());
    assert_eq!(rpc.rpc(), Some(&RpcError::internal()));
    assert_eq!(
      McpError::Http {
        status: 500,
        detail: "x".into()
      }
      .to_string(),
      "mcp http status 500: x"
    );
  }
}
