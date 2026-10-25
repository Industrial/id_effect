//! JSON-RPC 2.0 messages as MCP uses them, and the bounded decoder every transport reads
//! through ([`decode_message`]).
//!
//! MCP messages are single JSON objects (batching was removed in 2025-06-18): a request (`id` and
//! `method`), a notification (`method`, no `id`), a response (`id` and `result`) or an error
//! response (`error`, `id` absent or `null` when the request could not be read). `params` is an
//! object when present.

use std::fmt;

use serde_json::{Map, Value, json};

/// The `jsonrpc` member every message carries.
pub const JSONRPC_VERSION: &str = "2.0";

/// Default cap on one message: 1 MiB of JSON text (ADR 0015 B16, ADR 0013 R4).
pub const DEFAULT_MAX_MESSAGE_BYTES: usize = 1024 * 1024;

/// Longest `method` name and string request id accepted (bytes).
pub const MAX_NAME_BYTES: usize = 256;

/// JSON-RPC: the text is not JSON.
pub const PARSE_ERROR: i64 = -32700;
/// JSON-RPC: the JSON is not a valid request.
pub const INVALID_REQUEST: i64 = -32600;
/// JSON-RPC: the method does not exist or is not offered.
pub const METHOD_NOT_FOUND: i64 = -32601;
/// JSON-RPC: invalid method parameters (also an unknown tool, resource or prompt).
pub const INVALID_PARAMS: i64 = -32602;
/// JSON-RPC: internal error (a defect; the message carries no detail).
pub const INTERNAL_ERROR: i64 = -32603;
/// Implementation-defined server error: too many requests are in flight (ADR 0013 R4).
pub const SERVER_BUSY: i64 = -32000;
/// MCP: the Streamable HTTP routing headers disagree with the body.
pub const HEADER_MISMATCH: i64 = -32020;
/// MCP: the request needs a client capability it did not declare.
pub const MISSING_REQUIRED_CLIENT_CAPABILITY: i64 = -32021;
/// MCP: the requested protocol revision is not supported.
pub const UNSUPPORTED_PROTOCOL_VERSION: i64 = -32022;

/// A request id: a number or a string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RequestId {
  /// A numeric id.
  Number(i64),
  /// A string id.
  String(String),
}

impl RequestId {
  /// The id as JSON.
  pub fn to_json(&self) -> Value {
    match self {
      Self::Number(n) => Value::from(*n),
      Self::String(s) => Value::String(s.clone()),
    }
  }

  /// The id read from JSON (`None` for anything but an integer or a string of at most
  /// [`MAX_NAME_BYTES`]).
  pub fn from_json(value: &Value) -> Option<Self> {
    match value {
      Value::Number(n) => n.as_i64().map(Self::Number),
      Value::String(s) if s.len() <= MAX_NAME_BYTES => Some(Self::String(s.clone())),
      _ => None,
    }
  }
}

impl fmt::Display for RequestId {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Number(n) => write!(f, "{n}"),
      Self::String(s) => f.write_str(s),
    }
  }
}

/// A JSON-RPC error object.
#[derive(Debug, Clone, PartialEq)]
pub struct RpcError {
  /// The error code (see the constants of this module).
  pub code: i64,
  /// A short description.
  pub message: String,
  /// Structured detail.
  pub data: Option<Value>,
}

impl RpcError {
  /// An error with `code` and `message` and no data.
  pub fn new(code: i64, message: impl Into<String>) -> Self {
    Self {
      code,
      message: message.into(),
      data: None,
    }
  }

  /// Attaches `data`.
  #[must_use]
  pub fn with_data(mut self, data: Value) -> Self {
    self.data = Some(data);
    self
  }

  /// [`INVALID_PARAMS`] with `message`.
  pub fn invalid_params(message: impl Into<String>) -> Self {
    Self::new(INVALID_PARAMS, message)
  }

  /// [`METHOD_NOT_FOUND`] for `method`.
  pub fn method_not_found(method: &str) -> Self {
    Self::new(METHOD_NOT_FOUND, "Method not found").with_data(json!({ "method": method }))
  }

  /// [`INTERNAL_ERROR`] without detail (a defect never reaches the peer).
  pub fn internal() -> Self {
    Self::new(INTERNAL_ERROR, "Internal error")
  }

  /// The error object as JSON.
  pub fn to_json(&self) -> Value {
    let mut obj = Map::new();
    obj.insert("code".into(), Value::from(self.code));
    obj.insert("message".into(), Value::String(self.message.clone()));
    if let Some(data) = &self.data {
      obj.insert("data".into(), data.clone());
    }
    Value::Object(obj)
  }

  fn from_json(value: &Value) -> Option<Self> {
    let code = value.get("code")?.as_i64()?;
    let message = value.get("message")?.as_str()?.to_owned();
    Some(Self {
      code,
      message,
      data: value.get("data").cloned(),
    })
  }
}

impl fmt::Display for RpcError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{} ({})", self.message, self.code)
  }
}

impl std::error::Error for RpcError {}

/// A request: it expects exactly one response with the same `id`.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
  /// The id the response echoes.
  pub id: RequestId,
  /// The method.
  pub method: String,
  /// The parameters (an object), if any.
  pub params: Option<Value>,
}

/// A notification: no response.
#[derive(Debug, Clone, PartialEq)]
pub struct Notification {
  /// The method.
  pub method: String,
  /// The parameters (an object), if any.
  pub params: Option<Value>,
}

/// A successful response.
#[derive(Debug, Clone, PartialEq)]
pub struct Response {
  /// The id of the request answered.
  pub id: RequestId,
  /// The result.
  pub result: Value,
}

/// An error response.
#[derive(Debug, Clone, PartialEq)]
pub struct ErrorResponse {
  /// The id of the request answered (`None` when it could not be read).
  pub id: Option<RequestId>,
  /// The error.
  pub error: RpcError,
}

/// One JSON-RPC message.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
  /// A request.
  Request(Request),
  /// A notification.
  Notification(Notification),
  /// A successful response.
  Response(Response),
  /// An error response.
  Error(ErrorResponse),
}

impl Message {
  /// A request message.
  pub fn request(id: RequestId, method: impl Into<String>, params: Option<Value>) -> Self {
    Self::Request(Request {
      id,
      method: method.into(),
      params,
    })
  }

  /// A notification message.
  pub fn notification(method: impl Into<String>, params: Option<Value>) -> Self {
    Self::Notification(Notification {
      method: method.into(),
      params,
    })
  }

  /// A successful response.
  pub fn response(id: RequestId, result: Value) -> Self {
    Self::Response(Response { id, result })
  }

  /// An error response.
  pub fn error(id: Option<RequestId>, error: RpcError) -> Self {
    Self::Error(ErrorResponse { id, error })
  }

  /// The method of a request or notification.
  pub fn method(&self) -> Option<&str> {
    match self {
      Self::Request(r) => Some(&r.method),
      Self::Notification(n) => Some(&n.method),
      Self::Response(_) | Self::Error(_) => None,
    }
  }

  /// The message as JSON.
  pub fn to_json(&self) -> Value {
    let mut obj = Map::new();
    obj.insert("jsonrpc".into(), Value::String(JSONRPC_VERSION.into()));
    match self {
      Self::Request(r) => {
        obj.insert("id".into(), r.id.to_json());
        obj.insert("method".into(), Value::String(r.method.clone()));
        if let Some(params) = &r.params {
          obj.insert("params".into(), params.clone());
        }
      }
      Self::Notification(n) => {
        obj.insert("method".into(), Value::String(n.method.clone()));
        if let Some(params) = &n.params {
          obj.insert("params".into(), params.clone());
        }
      }
      Self::Response(r) => {
        obj.insert("id".into(), r.id.to_json());
        obj.insert("result".into(), r.result.clone());
      }
      Self::Error(e) => {
        obj.insert(
          "id".into(),
          e.id.as_ref().map_or(Value::Null, RequestId::to_json),
        );
        obj.insert("error".into(), e.error.to_json());
      }
    }
    Value::Object(obj)
  }

  /// The message as compact JSON text (one line: JSON escapes every newline).
  pub fn to_text(&self) -> String {
    self.to_json().to_string()
  }
}

/// Why bytes are not an MCP message.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DecodeError {
  /// The message is larger than the cap (nothing was parsed).
  TooLarge {
    /// The cap in bytes.
    max: usize,
  },
  /// The text is not JSON.
  Json(String),
  /// The JSON is not a JSON-RPC message MCP accepts (a batch, a wrong `jsonrpc`, a bad member).
  Invalid(String),
}

impl DecodeError {
  /// The JSON-RPC error a peer is answered with: [`PARSE_ERROR`] for text that is not JSON,
  /// [`INVALID_REQUEST`] otherwise.
  pub fn to_rpc_error(&self) -> RpcError {
    match self {
      Self::Json(_) => RpcError::new(PARSE_ERROR, "Parse error"),
      Self::TooLarge { max } => {
        RpcError::new(INVALID_REQUEST, format!("message exceeds {max} bytes"))
          .with_data(json!({ "maxBytes": max }))
      }
      Self::Invalid(why) => RpcError::new(INVALID_REQUEST, format!("Invalid request: {why}")),
    }
  }
}

impl fmt::Display for DecodeError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::TooLarge { max } => write!(f, "message exceeds {max} bytes"),
      Self::Json(e) => write!(f, "not JSON: {e}"),
      Self::Invalid(why) => write!(f, "not a JSON-RPC message: {why}"),
    }
  }
}

impl std::error::Error for DecodeError {}

/// Decodes one message of at most [`DEFAULT_MAX_MESSAGE_BYTES`] (the decoder of every MCP
/// transport, public for the `mcp_jsonrpc_message` fuzz target). Never panics.
///
/// ```
/// use id_effect_ai::mcp::{DecodeError, Message, RequestId, decode_message};
///
/// let msg = decode_message(br#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).unwrap();
/// assert!(matches!(msg, Message::Request(r) if r.id == RequestId::Number(1)));
/// assert!(matches!(decode_message(b"[]"), Err(DecodeError::Invalid(_))));
/// ```
pub fn decode_message(bytes: &[u8]) -> Result<Message, DecodeError> {
  decode_message_with_limit(bytes, DEFAULT_MAX_MESSAGE_BYTES)
}

/// [`decode_message`] with the cap `max_bytes`.
pub fn decode_message_with_limit(bytes: &[u8], max_bytes: usize) -> Result<Message, DecodeError> {
  if bytes.len() > max_bytes {
    return Err(DecodeError::TooLarge { max: max_bytes });
  }
  let value: Value = serde_json::from_slice(bytes).map_err(|e| DecodeError::Json(e.to_string()))?;
  message_from_json(value)
}

/// Classifies a parsed JSON value as a message.
pub(crate) fn message_from_json(value: Value) -> Result<Message, DecodeError> {
  let invalid = |why: &str| DecodeError::Invalid(why.to_owned());
  let Value::Object(mut obj) = value else {
    return Err(invalid(if value.is_array() {
      "batches are not supported"
    } else {
      "a message is a JSON object"
    }));
  };
  if obj.get("jsonrpc").and_then(Value::as_str) != Some(JSONRPC_VERSION) {
    return Err(invalid("`jsonrpc` must be \"2.0\""));
  }
  let id = match obj.remove("id") {
    None | Some(Value::Null) => None,
    Some(raw) => Some(RequestId::from_json(&raw).ok_or_else(|| invalid("bad `id`"))?),
  };
  if let Some(method) = obj.remove("method") {
    let Value::String(method) = method else {
      return Err(invalid("`method` must be a string"));
    };
    if method.is_empty() || method.len() > MAX_NAME_BYTES {
      return Err(invalid("`method` must be 1 to 256 bytes"));
    }
    let params = match obj.remove("params") {
      None | Some(Value::Null) => None,
      Some(p @ Value::Object(_)) => Some(p),
      Some(_) => return Err(invalid("`params` must be an object")),
    };
    return Ok(match id {
      Some(id) => Message::Request(Request { id, method, params }),
      None => Message::Notification(Notification { method, params }),
    });
  }
  if let Some(result) = obj.remove("result") {
    let id = id.ok_or_else(|| invalid("a response needs an `id`"))?;
    return Ok(Message::Response(Response { id, result }));
  }
  if let Some(error) = obj.remove("error") {
    let error = RpcError::from_json(&error).ok_or_else(|| invalid("bad `error` object"))?;
    return Ok(Message::Error(ErrorResponse { id, error }));
  }
  Err(invalid("neither a request, a notification nor a response"))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn requests_notifications_and_responses_round_trip() {
    let cases = [
      Message::request(RequestId::Number(7), "tools/list", Some(json!({}))),
      Message::request(RequestId::String("a".into()), "ping", None),
      Message::notification("notifications/cancelled", Some(json!({ "requestId": 7 }))),
      Message::notification("notifications/initialized", None),
      Message::response(RequestId::Number(7), json!({ "tools": [] })),
      Message::error(
        Some(RequestId::Number(7)),
        RpcError::invalid_params("bad").with_data(json!(1)),
      ),
      Message::error(None, RpcError::internal()),
    ];
    for msg in cases {
      let text = msg.to_text();
      assert_eq!(decode_message(text.as_bytes()).unwrap(), msg, "{text}");
    }
  }

  #[test]
  fn invalid_messages_are_refused() {
    let bad: [&[u8]; 14] = [
      b"not json",
      b"[]",
      b"1",
      br#"{"id":1,"method":"x"}"#,
      br#"{"jsonrpc":"1.0","id":1,"method":"x"}"#,
      br#"{"jsonrpc":"2.0","id":1.5,"method":"x"}"#,
      br#"{"jsonrpc":"2.0","id":[1],"method":"x"}"#,
      br#"{"jsonrpc":"2.0","id":1,"method":7}"#,
      br#"{"jsonrpc":"2.0","id":1,"method":""}"#,
      br#"{"jsonrpc":"2.0","id":1,"method":"x","params":[1]}"#,
      br#"{"jsonrpc":"2.0","result":{}}"#,
      br#"{"jsonrpc":"2.0","id":1,"error":{"code":"x"}}"#,
      br#"{"jsonrpc":"2.0","id":1}"#,
      br#"{"jsonrpc":"2.0","id":"#,
    ];
    for b in bad {
      let err = decode_message(b).unwrap_err();
      assert!(!err.to_string().is_empty());
      let rpc = err.to_rpc_error();
      assert!(rpc.code == PARSE_ERROR || rpc.code == INVALID_REQUEST);
    }
    let long = format!(
      r#"{{"jsonrpc":"2.0","id":1,"method":"{}"}}"#,
      "m".repeat(MAX_NAME_BYTES + 1)
    );
    assert!(decode_message(long.as_bytes()).is_err());
    let long_id = format!(
      r#"{{"jsonrpc":"2.0","id":"{}","method":"m"}}"#,
      "i".repeat(MAX_NAME_BYTES + 1)
    );
    assert!(decode_message(long_id.as_bytes()).is_err());
  }

  #[test]
  fn the_cap_is_checked_before_parsing() {
    let err = decode_message_with_limit(b"{}", 1).unwrap_err();
    assert_eq!(err, DecodeError::TooLarge { max: 1 });
    assert_eq!(err.to_rpc_error().code, INVALID_REQUEST);
    assert_eq!(err.to_string(), "message exceeds 1 bytes");
    let ok = br#"{"jsonrpc":"2.0","method":"x","params":null}"#;
    assert_eq!(
      decode_message(ok).unwrap(),
      Message::notification("x", None)
    );
  }

  #[test]
  fn ids_errors_and_methods_display() {
    assert_eq!(RequestId::Number(3).to_string(), "3");
    assert_eq!(RequestId::String("s".into()).to_string(), "s");
    assert_eq!(RequestId::from_json(&json!(true)), None);
    let e = RpcError::method_not_found("x/y");
    assert_eq!(e.code, METHOD_NOT_FOUND);
    assert_eq!(e.to_string(), "Method not found (-32601)");
    assert_eq!(RpcError::internal().code, INTERNAL_ERROR);
    assert_eq!(
      Message::request(RequestId::Number(1), "m", None).method(),
      Some("m")
    );
    assert_eq!(Message::notification("n", None).method(), Some("n"));
    assert_eq!(
      Message::response(RequestId::Number(1), json!(1)).method(),
      None
    );
    assert_eq!(Message::error(None, RpcError::internal()).method(), None);
  }
}
