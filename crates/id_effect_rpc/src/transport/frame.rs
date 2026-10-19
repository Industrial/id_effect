//! The frame codec of the multiplexed transports (design note §5): one [`Frame`] per WebSocket
//! message (text for JSON, binary for postcard), WebTransport record or mesh message.
//!
//! [`decode`] is the boundary every inbound frame crosses (and the fuzz target's entry point):
//! at most [`MAX_FRAME_BYTES`] of input, a known [`FrameKind`], no reserved flag bit, and the
//! members each kind needs — and no others:
//!
//! | Kind | `id` | Needs | May carry |
//! |---|---|---|---|
//! | `call` | ≥ 1 | `tag` (`[A-Za-z0-9_.-]{1,128}`) | `v`, `cursor` (a subscription's resume point), `payload` |
//! | `item` | ≥ 1 | — (an absent or `null` payload is the item `null`) | `cursor`, `payload` |
//! | `end`, `cancel` | ≥ 1 | — | — |
//! | `exit` | ≥ 1 | `payload` (a `WireExit`) | — |
//! | `credit` | ≥ 1 | `credits` ≥ 1 | — |
//! | `hello`, `resume` | 0 | `payload` | — |
//! | `ping`, `pong` | 0 | — | — |
//!
//! ```rust
//! use id_effect_rpc::codec::CodecId;
//! use id_effect_rpc::transport::frame::{self, FrameError};
//! use id_effect_rpc::wire::FrameKind;
//!
//! let call = frame::decode(br#"{"id":3,"kind":"call","tag":"Chat.room","payload":{"room":"a"}}"#, CodecId::Json).unwrap();
//! assert_eq!(call.kind, FrameKind::Call);
//! assert!(matches!(frame::decode(br#"{"id":3,"kind":"shout"}"#, CodecId::Json), Err(FrameError::Malformed(_))));
//! assert_eq!(frame::encode(&call, CodecId::Json).unwrap(), br#"{"id":3,"kind":"call","flags":0,"tag":"Chat.room","payload":{"room":"a"}}"#);
//! ```

use serde_json::Value;

use crate::codec::CodecId;
use crate::wire::{CallId, Frame, FrameKind, MAX_FRAME_BYTES};

/// Why bytes are not a frame (or a frame is not sendable).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
  /// More than [`MAX_FRAME_BYTES`] encoded (WebSocket: close 1009).
  #[error("frame of {len} bytes exceeds {MAX_FRAME_BYTES}")]
  TooLarge {
    /// The encoded length.
    len: usize,
  },
  /// Not a frame of this codec (an unknown `kind` lands here).
  #[error("malformed frame: {0}")]
  Malformed(String),
  /// A flag bit outside `END` and `ACK`.
  #[error("reserved frame flags {0:#04x}")]
  ReservedFlags(u8),
  /// A connection frame (`hello`, `resume`, `ping`, `pong`) on a call id, or a call frame on id 0.
  #[error("a `{kind:?}` frame on call id {id}")]
  WrongId {
    /// The kind.
    kind: FrameKind,
    /// The id it carried.
    id: u64,
  },
  /// A member the kind needs is absent, or one it may not carry is present.
  #[error("a `{kind:?}` frame {problem}")]
  Shape {
    /// The kind.
    kind: FrameKind,
    /// What is wrong.
    problem: &'static str,
  },
}

/// Whether `tag` is a procedure tag (`[A-Za-z0-9_.-]{1,128}`).
pub fn is_valid_tag(tag: &str) -> bool {
  (1..=128).contains(&tag.len())
    && tag
      .bytes()
      .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

/// Decode one frame with `codec`, then [`validate`] it.
pub fn decode(bytes: &[u8], codec: CodecId) -> Result<Frame, FrameError> {
  if bytes.len() > MAX_FRAME_BYTES {
    return Err(FrameError::TooLarge { len: bytes.len() });
  }
  let frame: Frame<Value> = codec
    .decode(bytes)
    .map_err(|err| FrameError::Malformed(err.to_string()))?;
  validate(&frame)?;
  Ok(frame)
}

/// Encode one frame with `codec`, refusing one over [`MAX_FRAME_BYTES`].
pub fn encode(frame: &Frame, codec: CodecId) -> Result<Vec<u8>, FrameError> {
  let bytes = codec
    .encode(frame)
    .map_err(|err| FrameError::Malformed(err.to_string()))?;
  if bytes.len() > MAX_FRAME_BYTES {
    return Err(FrameError::TooLarge { len: bytes.len() });
  }
  Ok(bytes)
}

/// The rules of the [module table](self): flags, id, and the members of each kind.
pub fn validate<P>(frame: &Frame<P>) -> Result<(), FrameError> {
  if frame.flags.has_reserved() {
    return Err(FrameError::ReservedFlags(frame.flags.0));
  }
  let kind = frame.kind;
  let connection = matches!(
    kind,
    FrameKind::Hello | FrameKind::Resume | FrameKind::Ping | FrameKind::Pong
  );
  if connection != (frame.id == CallId::CONNECTION) {
    return Err(FrameError::WrongId {
      kind,
      id: frame.id.get(),
    });
  }
  let shape = |problem| Err(FrameError::Shape { kind, problem });
  if kind != FrameKind::Call && (frame.tag.is_some() || frame.v.is_some()) {
    return shape("carries `tag` or `v` (call frames only)");
  }
  if kind != FrameKind::Credit && frame.credits.is_some() {
    return shape("carries `credits` (credit frames only)");
  }
  if !matches!(kind, FrameKind::Call | FrameKind::Item) && frame.cursor.is_some() {
    return shape("carries a `cursor` (call and item frames only)");
  }
  let payload = frame.payload.is_some();
  match kind {
    FrameKind::Call => match frame.tag.as_deref() {
      Some(tag) if is_valid_tag(tag) => Ok(()),
      Some(_) => shape("has a malformed `tag`"),
      None => shape("has no `tag`"),
    },
    FrameKind::Exit | FrameKind::Hello | FrameKind::Resume if !payload => shape("has no `payload`"),
    FrameKind::End | FrameKind::Cancel | FrameKind::Ping | FrameKind::Pong if payload => {
      shape("carries a `payload`")
    }
    FrameKind::Credit if frame.credits.unwrap_or(0) == 0 => shape("grants no `credits`"),
    _ => Ok(()),
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::wire::{Cursor, FrameFlags};

  fn json(text: &str) -> Result<Frame, FrameError> {
    decode(text.as_bytes(), CodecId::Json)
  }

  #[test]
  fn well_formed_frames_decode() {
    for text in [
      r#"{"id":1,"kind":"call","tag":"A.b","v":2,"payload":null}"#,
      r#"{"id":1,"kind":"call","tag":"A.b","cursor":"c1.AQAAAAAAAAAH"}"#,
      r#"{"id":1,"kind":"item","flags":1,"payload":3}"#,
      r#"{"id":1,"kind":"item","payload":null}"#,
      r#"{"id":1,"kind":"end","flags":1}"#,
      r#"{"id":1,"kind":"exit","payload":{"_tag":"Success","value":1}}"#,
      r#"{"id":1,"kind":"cancel"}"#,
      r#"{"id":1,"kind":"credit","credits":4}"#,
      r#"{"id":0,"kind":"hello","payload":{}}"#,
      r#"{"id":0,"kind":"resume","payload":{}}"#,
      r#"{"id":0,"kind":"ping"}"#,
      r#"{"id":0,"kind":"pong"}"#,
    ] {
      assert!(json(text).is_ok(), "{text}");
    }
  }

  #[test]
  fn ws_frame_decode_rejects_unknown_kind() {
    assert!(matches!(
      json(r#"{"id":1,"kind":"shout"}"#),
      Err(FrameError::Malformed(_))
    ));
    assert!(matches!(json("not json"), Err(FrameError::Malformed(_))));
    assert!(matches!(
      json(r#"{"id":9007199254740992,"kind":"cancel"}"#),
      Err(FrameError::Malformed(_))
    ));
  }

  #[test]
  fn bad_shapes_are_refused() {
    type Check = fn(&FrameError) -> bool;
    let cases: [(&str, Check); 13] = [
      (r#"{"id":1,"kind":"end","flags":4}"#, |e| {
        matches!(e, FrameError::ReservedFlags(4))
      }),
      (r#"{"id":0,"kind":"cancel"}"#, |e| {
        matches!(e, FrameError::WrongId { id: 0, .. })
      }),
      (r#"{"id":2,"kind":"ping"}"#, |e| {
        matches!(e, FrameError::WrongId { id: 2, .. })
      }),
      (r#"{"id":1,"kind":"call"}"#, |e| {
        e.to_string().contains("no `tag`")
      }),
      (r#"{"id":1,"kind":"call","tag":"a b"}"#, |e| {
        e.to_string().contains("malformed `tag`")
      }),
      (r#"{"id":1,"kind":"item","tag":"A.b","payload":1}"#, |e| {
        e.to_string().contains("call frames only")
      }),
      (r#"{"id":1,"kind":"item","credits":1,"payload":1}"#, |e| {
        e.to_string().contains("credit frames only")
      }),
      (
        r#"{"id":1,"kind":"exit","cursor":"c1.AQAAAAAAAAAH","payload":1}"#,
        |e| e.to_string().contains("`cursor`"),
      ),
      (r#"{"id":1,"kind":"exit"}"#, |e| {
        e.to_string().contains("no `payload`")
      }),
      (r#"{"id":0,"kind":"hello"}"#, |e| {
        e.to_string().contains("no `payload`")
      }),
      (r#"{"id":1,"kind":"cancel","payload":1}"#, |e| {
        e.to_string().contains("carries a `payload`")
      }),
      (r#"{"id":1,"kind":"credit","credits":0}"#, |e| {
        e.to_string().contains("no `credits`")
      }),
      (r#"{"id":1,"kind":"credit"}"#, |e| {
        e.to_string().contains("no `credits`")
      }),
    ];
    for (text, check) in cases {
      let err = json(text).unwrap_err();
      assert!(check(&err), "{text}: {err}");
    }
  }

  #[test]
  fn size_caps_both_ways() {
    let big = vec![b' '; MAX_FRAME_BYTES + 1];
    assert_eq!(
      decode(&big, CodecId::Json),
      Err(FrameError::TooLarge {
        len: MAX_FRAME_BYTES + 1
      })
    );
    let item = Frame::new(CallId::new(1).unwrap(), FrameKind::Item)
      .with_payload(Value::String("x".repeat(MAX_FRAME_BYTES)));
    assert!(matches!(
      encode(&item, CodecId::Json),
      Err(FrameError::TooLarge { .. })
    ));
    let ok = Frame::new(CallId::new(1).unwrap(), FrameKind::Item)
      .with_flags(FrameFlags::END)
      .with_cursor(Cursor::sequence(1))
      .with_payload(Value::from(1));
    let bytes = encode(&ok, CodecId::Json).unwrap();
    assert_eq!(decode(&bytes, CodecId::Json).unwrap(), ok);
  }

  #[test]
  fn tags_are_bounded() {
    assert!(is_valid_tag("Greeter.greet_v-2"));
    assert!(!is_valid_tag(""));
    assert!(!is_valid_tag(&"a".repeat(129)));
    assert!(!is_valid_tag("a/b"));
  }
}
