//! Typed error unions (ADR 0014 §2, C10; design note §4).
//!
//! A procedure's error type `E` is a tagged union on the wire: `{"_tag": "<Variant>", …fields}`
//! inside a `Failure` exit. [`RpcErrorUnion`] is what a handler error type implements (the
//! `#[rpc::service]` macro implements it for the error enums it sees): the wire value, the HTTP
//! status of a single call ([`RpcErrorStatus`], default **400**) and the union's JSON Schema for
//! the contract.
//!
//! The server checks every value before it is sent ([`typed_error_value`]): it must be an object
//! whose `_tag` is a string that is not one of the reserved [`RpcFailure`] tags. A value that
//! breaks the rule is treated as a defect and answered as `InternalError { id }`, so a buggy
//! implementation can never forge a reserved failure.
//!
//! ```rust
//! use axum::http::StatusCode;
//! use id_effect_rpc::error_union::{RpcErrorStatus, RpcErrorUnion};
//! use serde_json::{Value, json};
//!
//! enum TodoError {
//!   NotFound { id: u64 },
//!   Locked,
//! }
//!
//! impl RpcErrorStatus for TodoError {
//!   fn status(&self) -> StatusCode {
//!     match self {
//!       Self::NotFound { .. } => StatusCode::NOT_FOUND,
//!       Self::Locked => StatusCode::CONFLICT,
//!     }
//!   }
//! }
//!
//! impl RpcErrorUnion for TodoError {
//!   fn to_wire(&self) -> Value {
//!     match self {
//!       Self::NotFound { id } => json!({"_tag": "NotFound", "id": id}),
//!       Self::Locked => json!({"_tag": "Locked"}),
//!     }
//!   }
//! }
//!
//! assert_eq!(TodoError::NotFound { id: 7 }.to_wire()["_tag"], "NotFound");
//! assert_eq!(TodoError::Locked.status(), StatusCode::CONFLICT);
//! ```

use http::StatusCode;
use id_effect::Never;
use serde_json::{Value, json};

use crate::RpcError;
use crate::envelope::RpcErrorCode;
use crate::wire::RpcFailure;

/// The HTTP status of a single call failing with this error (a batch is always 200).
pub trait RpcErrorStatus {
  /// Default **400**.
  fn status(&self) -> StatusCode {
    StatusCode::BAD_REQUEST
  }
}

/// A handler error type: a tagged union on the wire.
pub trait RpcErrorUnion: RpcErrorStatus + Send + 'static {
  /// This error as a member of the union: a JSON object with a string `_tag` that is not one
  /// of [`RpcFailure::RESERVED_TAGS`].
  fn to_wire(&self) -> Value;

  /// Whether this error stands for a server fault rather than a contract failure: it is then
  /// logged and answered as `InternalError { id }`, without its fields (C10). Default `false`.
  fn is_internal(&self) -> bool {
    false
  }

  /// JSON Schema (2020-12) of the whole union for the contract's `error` member; `None` when
  /// the procedure declares no typed error.
  fn union_schema() -> Option<Value>
  where
    Self: Sized,
  {
    None
  }
}

impl RpcErrorStatus for Never {}

/// A procedure that cannot fail declares no union (`error: null`).
impl RpcErrorUnion for Never {
  fn to_wire(&self) -> Value {
    match *self {}
  }
}

impl RpcErrorStatus for RpcError {
  fn status(&self) -> StatusCode {
    RpcError::status(self)
  }
}

/// The labs [`RpcError`] as a union: `{"_tag": "InvalidArgument" | "NotFound" | "AlreadyExists" |
/// "Unauthenticated", "message": …}`. [`RpcError::internal`] is a server fault
/// ([`RpcErrorUnion::is_internal`]): the client sees `InternalError { id }` and never the message.
impl RpcErrorUnion for RpcError {
  fn to_wire(&self) -> Value {
    let envelope = self.envelope();
    json!({"_tag": code_tag(envelope.code), "message": envelope.message})
  }

  fn is_internal(&self) -> bool {
    self.envelope().code == RpcErrorCode::Internal
  }

  fn union_schema() -> Option<Value> {
    let variants: Vec<Value> = [
      RpcErrorCode::InvalidArgument,
      RpcErrorCode::NotFound,
      RpcErrorCode::AlreadyExists,
      RpcErrorCode::Unauthenticated,
    ]
    .into_iter()
    .map(|code| {
      json!({
        "type": "object",
        "properties": {
          "_tag": {"const": code_tag(code)},
          "message": {"type": "string"}
        },
        "required": ["_tag", "message"]
      })
    })
    .collect();
    Some(json!({ "oneOf": variants }))
  }
}

fn code_tag(code: RpcErrorCode) -> &'static str {
  match code {
    RpcErrorCode::InvalidArgument => "InvalidArgument",
    RpcErrorCode::NotFound => "NotFound",
    RpcErrorCode::AlreadyExists => "AlreadyExists",
    RpcErrorCode::Unauthenticated => "Unauthenticated",
    RpcErrorCode::Internal => "Internal",
  }
}

/// Why a typed error value was not sent.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UnionError {
  /// Not an object with a string `_tag`.
  #[error("a typed rpc error must be an object with a string `_tag`")]
  Untagged,
  /// The `_tag` is one of the reserved failures.
  #[error("typed rpc error uses the reserved tag `{0}`")]
  Reserved(String),
  /// [`RpcErrorUnion::is_internal`]: a server fault.
  #[error("the error is a server fault")]
  Internal,
}

/// The wire value and status of `error`, or why it must be answered as `InternalError`.
pub fn typed_error_value<E: RpcErrorUnion>(error: &E) -> Result<(Value, StatusCode), UnionError> {
  if error.is_internal() {
    return Err(UnionError::Internal);
  }
  let value = error.to_wire();
  match value.get("_tag").and_then(Value::as_str) {
    None => Err(UnionError::Untagged),
    Some(tag) if RpcFailure::is_reserved(tag) => Err(UnionError::Reserved(tag.to_owned())),
    Some(_) => Ok((value, error.status())),
  }
}

/// The wire value of a reserved failure.
pub fn failure_value(failure: &RpcFailure) -> Value {
  // A tagged enum of strings and string lists always serializes.
  serde_json::to_value(failure).unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
  use super::*;

  struct Forged(Value);

  impl RpcErrorStatus for Forged {}

  impl RpcErrorUnion for Forged {
    fn to_wire(&self) -> Value {
      self.0.clone()
    }
  }

  #[test]
  fn rpc_error_is_a_tagged_union_and_internal_is_a_fault() {
    let err = RpcError::not_found("no user");
    let (value, status) = typed_error_value(&err).unwrap();
    assert_eq!(value, json!({"_tag": "NotFound", "message": "no user"}));
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
      typed_error_value(&RpcError::internal("secret")),
      Err(UnionError::Internal)
    );
    assert_eq!(RpcError::internal("x").to_wire()["_tag"], "Internal");
    let schema = RpcError::union_schema().unwrap();
    assert_eq!(schema["oneOf"].as_array().unwrap().len(), 4);
    assert_eq!(
      schema["oneOf"][3]["properties"]["_tag"]["const"],
      "Unauthenticated"
    );
    assert!(Never::union_schema().is_none());
    for err in [
      RpcError::invalid_argument("a"),
      RpcError::already_exists("b"),
      RpcError::unauthenticated("c"),
    ] {
      assert!(typed_error_value(&err).is_ok());
    }
  }

  #[test]
  fn forged_or_untagged_values_are_refused() {
    let reserved = Forged(json!({"_tag": "InternalError", "id": "x"}));
    assert_eq!(
      typed_error_value(&reserved),
      Err(UnionError::Reserved("InternalError".into()))
    );
    assert_eq!(
      typed_error_value(&Forged(json!("NotFound"))),
      Err(UnionError::Untagged)
    );
    assert_eq!(
      typed_error_value(&Forged(json!({"_tag": 1}))),
      Err(UnionError::Untagged)
    );
    let ok = typed_error_value(&Forged(json!({"_tag": "Mine"}))).unwrap();
    assert_eq!(ok.1, StatusCode::BAD_REQUEST);
    for e in [
      UnionError::Untagged,
      UnionError::Reserved("Cancelled".into()),
      UnionError::Internal,
    ] {
      assert!(!e.to_string().is_empty());
    }
    assert_eq!(
      failure_value(&RpcFailure::DeadlineExceeded),
      json!({"_tag": "DeadlineExceeded"})
    );
  }
}
