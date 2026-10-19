//! The 0.12 wire envelope types (design note `r12-rpc-design.md` §2–§6): call ids, the call
//! envelope, the exit union, the reserved failures, stream frames, cursors and resume sessions.
//!
//! Every type is generic over its payload (`serde_json::Value` by default), so the JSON and
//! postcard codecs share one shape. Every transport speaks them: the HTTP routes
//! ([`crate::server`]) and the frame transports ([`crate::transport`]).
//!
//! ```rust
//! use id_effect_rpc::wire::{CallId, Cursor, Frame, FrameKind, WireExit, WireResult};
//! use serde_json::json;
//!
//! let result: WireResult = WireResult::new(Some(CallId::new(7).unwrap()), "Greeter.greet", WireExit::Success { value: json!("hi") });
//! assert_eq!(
//!   serde_json::to_value(&result).unwrap(),
//!   json!({"id": 7, "tag": "Greeter.greet", "exit": {"_tag": "Success", "value": "hi"}})
//! );
//!
//! let cursor = Cursor::events(7);
//! let text = cursor.to_string();
//! assert_eq!(text.parse::<Cursor>().unwrap(), cursor);
//!
//! let frame: Frame = Frame::new(CallId::new(3).unwrap(), FrameKind::Item).with_cursor(cursor);
//! assert_eq!(serde_json::to_value(&frame).unwrap()["cursor"], json!(text));
//! ```

use std::fmt;
use std::str::FromStr;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

/// Maximum encoded size of one frame (C3; over → WebSocket close 1009).
pub const MAX_FRAME_BYTES: usize = 64 * 1024;

/// Initial per-call credit window of a stream (items the receiver accepts before a `credit`).
pub const INITIAL_CREDITS: u32 = 32;

/// Default batch limits (C2): at most 50 calls and 1 MiB of body.
pub const MAX_BATCH_CALLS: usize = 50;

/// Default batch body cap in bytes (C2).
pub const MAX_BATCH_BYTES: usize = 1024 * 1024;

/// Largest call id: 2⁵³ − 1, so ids stay exact in JavaScript numbers.
pub const MAX_CALL_ID: u64 = (1 << 53) - 1;

/// A call id, `0..=`[`MAX_CALL_ID`]; `0` names the connection itself on frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "u64", into = "u64")]
pub struct CallId(u64);

/// A number outside `0..=`[`MAX_CALL_ID`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("call id {0} exceeds 2^53 - 1")]
pub struct CallIdOutOfRange(pub u64);

impl CallId {
  /// The connection's own id on frames (`hello`, `resume`, `ping`, `pong`).
  pub const CONNECTION: Self = Self(0);

  /// `id` when it is at most [`MAX_CALL_ID`].
  pub fn new(id: u64) -> Result<Self, CallIdOutOfRange> {
    if id > MAX_CALL_ID {
      Err(CallIdOutOfRange(id))
    } else {
      Ok(Self(id))
    }
  }

  /// The number.
  pub fn get(self) -> u64 {
    self.0
  }
}

impl TryFrom<u64> for CallId {
  type Error = CallIdOutOfRange;

  fn try_from(id: u64) -> Result<Self, Self::Error> {
    Self::new(id)
  }
}

impl From<CallId> for u64 {
  fn from(id: CallId) -> Self {
    id.0
  }
}

/// One call: `{"id"?, "tag", "v"?, "payload"}` (`POST /rpc`, one element of a batch).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireCall<P = Value> {
  /// Echoed in the result; required in a batch.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub id: Option<CallId>,
  /// `"<Service>.<procedure>"`.
  pub tag: String,
  /// Service version; absent means the newest served.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub v: Option<u32>,
  /// The input.
  pub payload: P,
}

impl<P> WireCall<P> {
  /// A call without an id or version.
  pub fn new(tag: impl Into<String>, payload: P) -> Self {
    Self {
      id: None,
      tag: tag.into(),
      v: None,
      payload,
    }
  }

  /// Set the call id.
  #[must_use]
  pub fn with_id(mut self, id: CallId) -> Self {
    self.id = Some(id);
    self
  }

  /// Set the service version.
  #[must_use]
  pub fn with_version(mut self, v: u32) -> Self {
    self.v = Some(v);
    self
  }
}

/// A batch body: a JSON array of calls, each with a distinct id (r12-e12-1-batching).
pub type Batch<P = Value> = Vec<WireCall<P>>;

pub use crate::batch::{BatchError, decode_batch};

/// How a call ended: `{"_tag":"Success","value":…}` or `{"_tag":"Failure","error":{"_tag":…}}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "_tag")]
pub enum WireExit<T = Value, E = Value> {
  /// The procedure's output.
  Success {
    /// The output value.
    value: T,
  },
  /// A typed error (a tagged union itself) or one of the [`RpcFailure`]s.
  Failure {
    /// The error.
    error: E,
  },
}

impl<T, E> WireExit<T, E> {
  /// Whether the call succeeded.
  pub fn is_success(&self) -> bool {
    matches!(self, Self::Success { .. })
  }
}

/// The result of one call: `{"id"?, "tag", "exit"}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireResult<T = Value, E = Value> {
  /// The call's id, echoed.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub id: Option<CallId>,
  /// The call's tag, echoed.
  pub tag: String,
  /// How it ended.
  pub exit: WireExit<T, E>,
}

impl<T, E> WireResult<T, E> {
  /// A result for `tag`.
  pub fn new(id: Option<CallId>, tag: impl Into<String>, exit: WireExit<T, E>) -> Self {
    Self {
      id,
      tag: tag.into(),
      exit,
    }
  }
}

/// One Schema issue of an [`RpcFailure::InvalidInput`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputIssue {
  /// JSON Pointer into the input (`""` = the input itself).
  pub path: String,
  /// Why it failed.
  pub message: String,
}

/// The reserved failures: they may appear in any procedure's `Failure` without being declared,
/// and no user error variant may take their names ([`RpcFailure::RESERVED_TAGS`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "_tag")]
pub enum RpcFailure {
  /// A defect or unexpected interrupt: the correlation id only, never a message (C10).
  InternalError {
    /// The correlation id (`x-correlation-id`) to find the defect in the logs.
    id: String,
  },
  /// The input failed Schema decode.
  InvalidInput {
    /// Every issue at once.
    issues: Vec<InputIssue>,
  },
  /// The server-capped deadline expired.
  DeadlineExceeded,
  /// The client cancelled or disconnected.
  Cancelled,
  /// A per-call limit (items, bytes, credits) was exceeded.
  ResourceExhausted {
    /// Which limit.
    limit: String,
  },
  /// A resume came after the replay buffer overran.
  ResumeExpired,
}

impl RpcFailure {
  /// The `_tag`s user error variants may not use.
  pub const RESERVED_TAGS: [&'static str; 6] = [
    "InternalError",
    "InvalidInput",
    "DeadlineExceeded",
    "Cancelled",
    "ResourceExhausted",
    "ResumeExpired",
  ];

  /// Whether `tag` is reserved.
  pub fn is_reserved(tag: &str) -> bool {
    Self::RESERVED_TAGS.contains(&tag)
  }

  /// This failure's `_tag`.
  pub fn tag(&self) -> &'static str {
    match self {
      Self::InternalError { .. } => "InternalError",
      Self::InvalidInput { .. } => "InvalidInput",
      Self::DeadlineExceeded => "DeadlineExceeded",
      Self::Cancelled => "Cancelled",
      Self::ResourceExhausted { .. } => "ResourceExhausted",
      Self::ResumeExpired => "ResumeExpired",
    }
  }

  /// The HTTP status of a single call failing this way (a batch is always 200).
  pub fn status(&self) -> http::StatusCode {
    use http::StatusCode;
    match self {
      Self::InternalError { .. } => StatusCode::INTERNAL_SERVER_ERROR,
      Self::InvalidInput { .. } => StatusCode::UNPROCESSABLE_ENTITY,
      Self::DeadlineExceeded => StatusCode::GATEWAY_TIMEOUT,
      // 499 "client closed request": only ever logged, the client is gone.
      Self::Cancelled => StatusCode::from_u16(499).unwrap_or(StatusCode::BAD_REQUEST),
      Self::ResourceExhausted { .. } => StatusCode::TOO_MANY_REQUESTS,
      Self::ResumeExpired => StatusCode::GONE,
    }
  }
}

/// What a [`Frame`] does (postcard: the variant index, in this order, never reordered).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrameKind {
  /// Client opens a call.
  Call,
  /// One stream item, either direction.
  Item,
  /// Half-close without an item.
  End,
  /// The call's final [`WireExit`]; closes the call.
  Exit,
  /// Client interrupts the call.
  Cancel,
  /// Grant more items.
  Credit,
  /// Server greets the connection (id 0): session, codec, limits.
  Hello,
  /// Client resumes calls of a session (id 0).
  Resume,
  /// Application heartbeat (id 0).
  Ping,
  /// Heartbeat answer (id 0).
  Pong,
}

/// The `flags` bit set of a [`Frame`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FrameFlags(pub u8);

impl FrameFlags {
  /// No flag.
  pub const NONE: Self = Self(0);
  /// Last item from this side (half-close).
  pub const END: Self = Self(1);
  /// The item needs a credit back.
  pub const ACK: Self = Self(1 << 1);

  /// Whether every bit of `other` is set.
  pub fn contains(self, other: Self) -> bool {
    self.0 & other.0 == other.0
  }

  /// The union of both sets.
  #[must_use]
  pub fn with(self, other: Self) -> Self {
    Self(self.0 | other.0)
  }

  /// Whether a bit outside [`FrameFlags::END`] and [`FrameFlags::ACK`] is set (refused by
  /// decoders).
  pub fn has_reserved(self) -> bool {
    self.0 & !(Self::END.0 | Self::ACK.0) != 0
  }
}

/// One frame of a multiplexed connection (WebSocket message, WebTransport record, NDJSON line,
/// mesh message); at most [`MAX_FRAME_BYTES`] encoded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Frame<P = Value> {
  /// The call, or [`CallId::CONNECTION`].
  pub id: CallId,
  /// What the frame does.
  pub kind: FrameKind,
  /// Flag bits.
  #[serde(default)]
  pub flags: FrameFlags,
  /// `call` only: the procedure tag.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub tag: Option<String>,
  /// `call` only: the service version.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub v: Option<u32>,
  /// Server `item`s of resumable calls: the position to resume after.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub cursor: Option<Cursor>,
  /// `credit` only: how many more items the sender accepts.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub credits: Option<u32>,
  /// The input, item, exit or connection object.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub payload: Option<P>,
}

impl<P> Frame<P> {
  /// A frame with no flags and no optional members.
  pub fn new(id: CallId, kind: FrameKind) -> Self {
    Self {
      id,
      kind,
      flags: FrameFlags::NONE,
      tag: None,
      v: None,
      cursor: None,
      credits: None,
      payload: None,
    }
  }

  /// A `call` frame opening `tag` with `payload`.
  pub fn call(id: CallId, tag: impl Into<String>, payload: P) -> Self {
    let mut frame = Self::new(id, FrameKind::Call);
    frame.tag = Some(tag.into());
    frame.payload = Some(payload);
    frame
  }

  /// A `credit` frame granting `credits` items.
  pub fn credit(id: CallId, credits: u32) -> Self {
    let mut frame = Self::new(id, FrameKind::Credit);
    frame.credits = Some(credits);
    frame
  }

  /// Set the flags.
  #[must_use]
  pub fn with_flags(mut self, flags: FrameFlags) -> Self {
    self.flags = flags;
    self
  }

  /// Set the cursor.
  #[must_use]
  pub fn with_cursor(mut self, cursor: Cursor) -> Self {
    self.cursor = Some(cursor);
    self
  }

  /// Set the payload.
  #[must_use]
  pub fn with_payload(mut self, payload: P) -> Self {
    self.payload = Some(payload);
    self
  }
}

/// The `hello` frame's payload: the server's choices for the connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
  /// The connection's resume session.
  pub session: SessionId,
  /// The negotiated codec's subprotocol name (`idrpc.json.v1`).
  pub codec: String,
  /// [`MAX_FRAME_BYTES`] or less.
  pub max_frame: u32,
  /// The initial credit window ([`INITIAL_CREDITS`]).
  pub window: u32,
}

/// One call of a [`Resume`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeCall {
  /// The call to resume.
  pub id: CallId,
  /// The last cursor the client saw.
  pub cursor: Cursor,
}

/// The `resume` frame's payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resume {
  /// The session to resume, bound to the principal that created it (C8).
  pub session: SessionId,
  /// The calls to replay from their cursors.
  pub calls: Vec<ResumeCall>,
}

/// What a [`Cursor`] position counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CursorSource {
  /// An `id_effect_events` global position (subscriptions).
  Events,
  /// A per-session message sequence (stream and bidi resume).
  Sequence,
}

impl CursorSource {
  fn byte(self) -> u8 {
    match self {
      Self::Events => 1,
      Self::Sequence => 2,
    }
  }

  fn from_byte(b: u8) -> Option<Self> {
    match b {
      1 => Some(Self::Events),
      2 => Some(Self::Sequence),
      _ => None,
    }
  }
}

/// A resume position: `"c1." + base64url_nopad(source: u8, position: u64 big-endian)`.
///
/// Opaque to clients and not a capability: resume authority comes from the [`SessionId`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Cursor {
  /// What the position counts.
  pub source: CursorSource,
  /// The position.
  pub position: u64,
}

/// The longest cursor text a decoder reads (fuzzed: `Last-Event-ID`, resume frames).
pub const MAX_CURSOR_LEN: usize = 64;

const CURSOR_PREFIX: &str = "c1.";

/// A cursor that does not decode.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CursorError {
  /// Longer than [`MAX_CURSOR_LEN`].
  #[error("cursor longer than {MAX_CURSOR_LEN} bytes")]
  TooLong,
  /// Not `c1.` followed by base64url of 9 bytes.
  #[error("malformed cursor")]
  Malformed,
  /// A source byte this version does not know.
  #[error("unknown cursor source {0}")]
  UnknownSource(u8),
}

impl Cursor {
  /// An `id_effect_events` global position.
  pub fn events(position: u64) -> Self {
    Self {
      source: CursorSource::Events,
      position,
    }
  }

  /// A per-session sequence number.
  pub fn sequence(position: u64) -> Self {
    Self {
      source: CursorSource::Sequence,
      position,
    }
  }

  /// Decode cursor text, bounded by [`MAX_CURSOR_LEN`], refusing padding and trailing data.
  pub fn decode(text: &str) -> Result<Self, CursorError> {
    if text.len() > MAX_CURSOR_LEN {
      return Err(CursorError::TooLong);
    }
    let body = text
      .strip_prefix(CURSOR_PREFIX)
      .ok_or(CursorError::Malformed)?;
    let bytes = URL_SAFE_NO_PAD
      .decode(body)
      .map_err(|_| CursorError::Malformed)?;
    let [source, rest @ ..] = bytes.as_slice() else {
      return Err(CursorError::Malformed);
    };
    let position: [u8; 8] = rest.try_into().map_err(|_| CursorError::Malformed)?;
    let source = CursorSource::from_byte(*source).ok_or(CursorError::UnknownSource(*source))?;
    Ok(Self {
      source,
      position: u64::from_be_bytes(position),
    })
  }
}

impl fmt::Display for Cursor {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let mut bytes = [0u8; 9];
    bytes[0] = self.source.byte();
    bytes[1..].copy_from_slice(&self.position.to_be_bytes());
    write!(f, "{CURSOR_PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes))
  }
}

impl FromStr for Cursor {
  type Err = CursorError;

  fn from_str(s: &str) -> Result<Self, Self::Err> {
    Self::decode(s)
  }
}

impl Serialize for Cursor {
  fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
    s.collect_str(self)
  }
}

impl<'de> Deserialize<'de> for Cursor {
  fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
    let text = String::deserialize(d)?;
    Self::decode(&text).map_err(serde::de::Error::custom)
  }
}

/// A resume session id: 128 random bits, 22 base64url characters on the wire. `Debug` does not
/// print it.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionId([u8; 16]);

/// Session id text that does not decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("malformed session id")]
pub struct SessionIdError;

impl SessionId {
  /// A fresh id from the operating system's randomness.
  pub fn random() -> Self {
    Self(rand::random())
  }

  /// The id's bytes.
  pub fn as_bytes(&self) -> &[u8; 16] {
    &self.0
  }

  /// Decode 22 base64url characters.
  pub fn decode(text: &str) -> Result<Self, SessionIdError> {
    if text.len() != 22 {
      return Err(SessionIdError);
    }
    let bytes = URL_SAFE_NO_PAD.decode(text).map_err(|_| SessionIdError)?;
    let bytes: [u8; 16] = bytes.try_into().map_err(|_| SessionIdError)?;
    Ok(Self(bytes))
  }
}

impl From<[u8; 16]> for SessionId {
  fn from(bytes: [u8; 16]) -> Self {
    Self(bytes)
  }
}

impl fmt::Debug for SessionId {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("SessionId(..)")
  }
}

impl fmt::Display for SessionId {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(&URL_SAFE_NO_PAD.encode(self.0))
  }
}

impl FromStr for SessionId {
  type Err = SessionIdError;

  fn from_str(s: &str) -> Result<Self, Self::Err> {
    Self::decode(s)
  }
}

impl Serialize for SessionId {
  fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
    s.collect_str(self)
  }
}

impl<'de> Deserialize<'de> for SessionId {
  fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
    let text = String::deserialize(d)?;
    Self::decode(&text).map_err(serde::de::Error::custom)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use serde_json::json;

  fn id(n: u64) -> CallId {
    CallId::new(n).unwrap()
  }

  #[test]
  fn call_ids_stay_exact_in_javascript() {
    assert_eq!(id(MAX_CALL_ID).get(), MAX_CALL_ID);
    assert_eq!(
      CallId::new(MAX_CALL_ID + 1),
      Err(CallIdOutOfRange(MAX_CALL_ID + 1))
    );
    assert_eq!(CallId::CONNECTION.get(), 0);
    assert!(serde_json::from_value::<CallId>(json!(MAX_CALL_ID + 1)).is_err());
    assert_eq!(serde_json::to_value(id(9)).unwrap(), json!(9));
    assert_eq!(u64::from(CallId::try_from(4).unwrap()), 4);
    assert!(
      CallIdOutOfRange(1 << 60)
        .to_string()
        .contains("exceeds 2^53")
    );
  }

  #[test]
  fn call_envelope_round_trips_and_omits_unset_members() {
    let call = WireCall::new("Greeter.greet", json!({"name": "Ada"}))
      .with_id(id(7))
      .with_version(2);
    let v = serde_json::to_value(&call).unwrap();
    assert_eq!(
      v,
      json!({"id": 7, "tag": "Greeter.greet", "v": 2, "payload": {"name": "Ada"}})
    );
    assert_eq!(serde_json::from_value::<WireCall>(v).unwrap(), call);
    let bare = serde_json::to_value(WireCall::new("A.b", json!(null))).unwrap();
    assert_eq!(bare, json!({"tag": "A.b", "payload": null}));
    let batch: Batch = serde_json::from_value(json!([
      {"id": 1, "tag": "A.b", "payload": 1},
      {"id": 2, "tag": "A.c", "payload": 2}
    ]))
    .unwrap();
    assert_eq!(batch.len(), 2);
    assert_eq!((MAX_BATCH_CALLS, MAX_BATCH_BYTES), (50, 1 << 20));
  }

  #[test]
  fn exits_are_tagged_unions() {
    let ok: WireResult = WireResult::new(None, "A.b", WireExit::Success { value: json!(1) });
    assert!(ok.exit.is_success());
    assert_eq!(
      serde_json::to_value(&ok).unwrap(),
      json!({"tag": "A.b", "exit": {"_tag": "Success", "value": 1}})
    );
    let failed: WireResult<Value, RpcFailure> = WireResult::new(
      Some(id(3)),
      "A.b",
      WireExit::Failure {
        error: RpcFailure::InternalError { id: "cid".into() },
      },
    );
    assert!(!failed.exit.is_success());
    let v = serde_json::to_value(&failed).unwrap();
    assert_eq!(
      v,
      json!({"id": 3, "tag": "A.b", "exit": {"_tag": "Failure", "error": {"_tag": "InternalError", "id": "cid"}}})
    );
    let back: WireResult<Value, RpcFailure> = serde_json::from_value(v).unwrap();
    assert_eq!(back, failed);
  }

  #[test]
  fn reserved_failures_have_tags_and_statuses() {
    let all = [
      RpcFailure::InternalError { id: "x".into() },
      RpcFailure::InvalidInput {
        issues: vec![InputIssue {
          path: "/name".into(),
          message: "expected string".into(),
        }],
      },
      RpcFailure::DeadlineExceeded,
      RpcFailure::Cancelled,
      RpcFailure::ResourceExhausted {
        limit: "items".into(),
      },
      RpcFailure::ResumeExpired,
    ];
    let statuses: Vec<u16> = all.iter().map(|f| f.status().as_u16()).collect();
    assert_eq!(statuses, [500, 422, 504, 499, 429, 410]);
    for (failure, tag) in all.iter().zip(RpcFailure::RESERVED_TAGS) {
      assert_eq!(failure.tag(), tag);
      assert!(RpcFailure::is_reserved(tag));
      let v = serde_json::to_value(failure).unwrap();
      assert_eq!(v["_tag"], tag);
      assert_eq!(&serde_json::from_value::<RpcFailure>(v).unwrap(), failure);
    }
    assert!(!RpcFailure::is_reserved("NotFound"));
  }

  #[test]
  fn frames_carry_kind_flags_cursor_and_credits() {
    let call: Frame = Frame::call(id(3), "Chat.room", json!({"room": "a"}));
    assert_eq!(
      serde_json::to_value(&call).unwrap(),
      json!({"id": 3, "kind": "call", "flags": 0, "tag": "Chat.room", "payload": {"room": "a"}})
    );
    let item: Frame = Frame::new(id(3), FrameKind::Item)
      .with_flags(FrameFlags::END.with(FrameFlags::ACK))
      .with_cursor(Cursor::sequence(7))
      .with_payload(json!("hi"));
    let v = serde_json::to_value(&item).unwrap();
    assert_eq!(v["flags"], 3);
    assert_eq!(serde_json::from_value::<Frame>(v).unwrap(), item);
    let credit: Frame = Frame::credit(CallId::CONNECTION, 32);
    assert_eq!(credit.credits, Some(INITIAL_CREDITS));
    let minimal: Frame = serde_json::from_value(json!({"id": 1, "kind": "cancel"})).unwrap();
    assert_eq!(minimal.flags, FrameFlags::NONE);
    assert!(serde_json::from_value::<Frame>(json!({"id": 1, "kind": "shout"})).is_err());
    let kinds = [
      FrameKind::End,
      FrameKind::Exit,
      FrameKind::Hello,
      FrameKind::Resume,
      FrameKind::Ping,
      FrameKind::Pong,
    ];
    let names: Vec<Value> = kinds
      .iter()
      .map(|k| serde_json::to_value(k).unwrap())
      .collect();
    assert_eq!(
      names,
      [
        json!("end"),
        json!("exit"),
        json!("hello"),
        json!("resume"),
        json!("ping"),
        json!("pong")
      ]
    );
    assert_eq!(MAX_FRAME_BYTES, 65_536);
  }

  #[test]
  fn flags_test_bits_and_reserved_bits() {
    let both = FrameFlags::END.with(FrameFlags::ACK);
    assert!(both.contains(FrameFlags::END));
    assert!(!FrameFlags::END.contains(FrameFlags::ACK));
    assert!(!both.has_reserved());
    assert!(FrameFlags(4).has_reserved());
    assert_eq!(FrameFlags::default(), FrameFlags::NONE);
  }

  #[test]
  fn cursors_encode_decode_and_refuse_garbage() {
    let c = Cursor::events(7);
    let text = c.to_string();
    assert_eq!(text, "c1.AQAAAAAAAAAH");
    assert_eq!(text.len(), 15);
    assert_eq!(text.parse::<Cursor>().unwrap(), c);
    let s = Cursor::sequence(u64::MAX);
    assert_eq!(Cursor::decode(&s.to_string()).unwrap(), s);
    assert_eq!(Cursor::decode(&"c".repeat(65)), Err(CursorError::TooLong));
    assert_eq!(
      Cursor::decode("c2.AQAAAAAAAAAH"),
      Err(CursorError::Malformed)
    );
    assert_eq!(Cursor::decode("c1.!!"), Err(CursorError::Malformed));
    assert_eq!(Cursor::decode("c1."), Err(CursorError::Malformed));
    assert_eq!(Cursor::decode("c1.AQAA"), Err(CursorError::Malformed));
    assert_eq!(
      Cursor::decode("c1.AQAAAAAAAAAHAA"),
      Err(CursorError::Malformed)
    );
    assert_eq!(
      Cursor::decode("c1.AQAAAAAAAAAH="),
      Err(CursorError::Malformed)
    );
    assert_eq!(
      Cursor::decode("c1.CQAAAAAAAAAH"),
      Err(CursorError::UnknownSource(9))
    );
    assert!(CursorError::UnknownSource(9).to_string().contains('9'));
    assert_eq!(serde_json::to_value(c).unwrap(), json!(text));
    assert!(serde_json::from_value::<Cursor>(json!("nope")).is_err());
    assert!(serde_json::from_value::<Cursor>(json!(1)).is_err());
  }

  #[test]
  fn session_ids_are_random_hidden_and_round_trip() {
    let a = SessionId::random();
    let b = SessionId::random();
    assert_ne!(a, b);
    assert_eq!(format!("{a:?}"), "SessionId(..)");
    let text = a.to_string();
    assert_eq!(text.len(), 22);
    assert_eq!(text.parse::<SessionId>().unwrap(), a);
    assert_eq!(SessionId::from(*a.as_bytes()), a);
    assert_eq!(SessionId::decode("short"), Err(SessionIdError));
    assert_eq!(SessionId::decode(&"!".repeat(22)), Err(SessionIdError));
    assert!(SessionIdError.to_string().contains("session"));
    let v = serde_json::to_value(a).unwrap();
    assert_eq!(serde_json::from_value::<SessionId>(v).unwrap(), a);
    assert!(serde_json::from_value::<SessionId>(json!("x")).is_err());
    assert!(serde_json::from_value::<SessionId>(json!(1)).is_err());
  }

  #[test]
  fn hello_and_resume_payloads_round_trip() {
    let session = SessionId::from([7u8; 16]);
    let hello = Hello {
      session,
      codec: "idrpc.json.v1".into(),
      max_frame: MAX_FRAME_BYTES as u32,
      window: INITIAL_CREDITS,
    };
    let v = serde_json::to_value(&hello).unwrap();
    assert_eq!(v["codec"], "idrpc.json.v1");
    assert_eq!(serde_json::from_value::<Hello>(v).unwrap(), hello);
    let resume = Resume {
      session,
      calls: vec![ResumeCall {
        id: id(3),
        cursor: Cursor::sequence(4),
      }],
    };
    let v = serde_json::to_value(&resume).unwrap();
    assert_eq!(serde_json::from_value::<Resume>(v).unwrap(), resume);
  }
}
