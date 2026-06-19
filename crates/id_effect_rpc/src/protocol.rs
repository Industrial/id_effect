//! Tagged RPC wire messages for the HTTP+JSON protocol (`@effect/rpc`-shaped).

use serde::{Deserialize, Serialize};

use crate::envelope::RpcEnvelope;

/// Default single-dispatch path for tagged RPC requests.
pub const RPC_DISPATCH_PATH: &str = "/rpc";

/// JSON request body for a tagged RPC call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcWireRequest {
  /// Operation tag (matches [`crate::registry::RpcMethodEntry::tag`]).
  pub tag: String,
  /// JSON payload decoded by the method's request schema.
  pub payload: serde_json::Value,
}

/// JSON response body for a tagged RPC call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RpcWireResponse {
  /// Successful result for `tag`.
  Success {
    /// Echo of the operation tag.
    tag: String,
    /// JSON success value encoded by the method's response schema.
    success: serde_json::Value,
  },
  /// Structured RPC failure for `tag`.
  Failure {
    /// Echo of the operation tag.
    tag: String,
    /// Wire error envelope (HTTP status is chosen separately at the Axum edge).
    failure: RpcEnvelope,
  },
}

impl RpcWireResponse {
  /// Build a success response.
  #[inline]
  pub fn success(tag: impl Into<String>, success: serde_json::Value) -> Self {
    Self::Success {
      tag: tag.into(),
      success,
    }
  }

  /// Build a failure response from an [`crate::RpcError`] envelope.
  #[inline]
  pub fn failure(tag: impl Into<String>, failure: RpcEnvelope) -> Self {
    Self::Failure {
      tag: tag.into(),
      failure,
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::envelope::{RpcEnvelope, RpcErrorCode};

  #[test]
  fn wire_request_round_trips_json() {
    let req = RpcWireRequest {
      tag: "greet".to_owned(),
      payload: serde_json::json!({"name": "Ada", "enthusiasm": 3}),
    };
    let bytes = serde_json::to_vec(&req).expect("ser");
    let back: RpcWireRequest = serde_json::from_slice(&bytes).expect("de");
    assert_eq!(back, req);
  }

  #[test]
  fn wire_response_success_and_failure_tags() {
    let ok = RpcWireResponse::success("greet", serde_json::json!({"message": "hi"}));
    let v: serde_json::Value = serde_json::to_value(&ok).expect("ser");
    assert_eq!(v["kind"], "success");
    assert_eq!(v["tag"], "greet");

    let fail = RpcWireResponse::failure(
      "greet",
      RpcEnvelope {
        code: RpcErrorCode::InvalidArgument,
        message: "bad".to_owned(),
        correlation_id: None,
        details: None,
      },
    );
    let v: serde_json::Value = serde_json::to_value(&fail).expect("ser");
    assert_eq!(v["kind"], "failure");
    assert_eq!(v["failure"]["code"], "invalid_argument");
  }
}
