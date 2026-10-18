//! Wire protocol ([`PROTOCOL_VERSION`]): versioned, postcard-encoded frames.
//!
//! Every frame on a connection is a [`Frame`]. The first bytes exchanged
//! after transport connect are handshake frames; only after a successful
//! handshake do data frames flow. The envelope payload (`Frame::Data`) is
//! opaque to this layer — the process layer validates it through Schema
//! at the trust boundary (ADR 0009 § wire protocol).

use bytes::Bytes;
use serde::{Deserialize, Serialize};

use crate::id::NodeId;

/// Protocol version spoken by this build. Nodes refuse to connect across
/// versions (no cross-version clusters — an explicit non-goal).
///
/// Version 2 (0.10.1) binds the cookie proof to the whole handshake
/// transcript and the TLS channel and widens the nonces to 128 bits; 0.10.0
/// nodes (version 1) and 0.10.1 nodes refuse each other with
/// [`HandshakeError::VersionMismatch`](crate::handshake::HandshakeError::VersionMismatch).
pub const PROTOCOL_VERSION: u16 = 2;

/// Everything that can travel on a connection.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Frame {
  /// Handshake sub-protocol (see [`crate::handshake`]).
  Handshake(HandshakeMsg),
  /// Opaque application payload (process-layer envelope).
  Data(Vec<u8>),
  /// Liveness probe.
  Ping(u64),
  /// Liveness reply.
  Pong(u64),
  /// Orderly close with a human-readable reason.
  Close(String),
}

/// Cookie-authenticated mutual handshake (HMAC-SHA256 challenge/response;
/// BEAM's cookie scheme with a modern MAC).
///
/// These are the protocol v1 shapes, kept for compatibility. From protocol
/// v2 the handshake ([`crate::handshake`]) uses its own codec with the same
/// layout but 128-bit nonces, and its digests MAC the whole transcript; this
/// type still decodes v2 `Hello`, `Welcome` and `Rejected` frames, but not
/// a v2 `Challenge` or `ChallengeReply` whose nonce exceeds `u64::MAX`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum HandshakeMsg {
  /// Initiator → responder: who I am, what I speak.
  Hello {
    /// Protocol version of the initiator.
    version: u16,
    /// Initiator identity.
    node: NodeId,
  },
  /// Responder → initiator: who I am + a nonce to prove cookie knowledge.
  Challenge {
    /// Protocol version of the responder.
    version: u16,
    /// Responder identity.
    node: NodeId,
    /// Nonce the initiator must MAC.
    nonce: u64,
  },
  /// Initiator → responder: MAC over the responder's nonce + my own nonce.
  ChallengeReply {
    /// `HMAC(cookie, responder_nonce ‖ initiator_name)`.
    digest: [u8; 32],
    /// Nonce the responder must MAC back (mutual auth).
    nonce: u64,
  },
  /// Responder → initiator: MAC over the initiator's nonce. Connection is
  /// established once the initiator verifies this.
  Welcome {
    /// `HMAC(cookie, initiator_nonce ‖ responder_name)`.
    digest: [u8; 32],
  },
  /// Either side: handshake refused (bad cookie, version skew, …).
  Rejected {
    /// Human-readable reason (never contains secrets).
    reason: String,
  },
}

/// Wire encode/decode failure.
#[derive(Debug, thiserror::Error)]
pub enum WireError {
  /// Postcard (de)serialization failed.
  #[error("wire codec error: {0}")]
  Codec(#[from] postcard::Error),
}

/// Encode a frame to bytes (postcard).
pub fn encode(frame: &Frame) -> Result<Bytes, WireError> {
  Ok(Bytes::from(postcard::to_allocvec(frame)?))
}

/// Decode a frame from bytes (postcard). Unknown/garbage input fails
/// cleanly — the caller must treat it as a protocol violation and close.
pub fn decode(bytes: &[u8]) -> Result<Frame, WireError> {
  Ok(postcard::from_bytes(bytes)?)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn frames_round_trip() {
    let frames = vec![
      Frame::Handshake(HandshakeMsg::Hello {
        version: PROTOCOL_VERSION,
        node: NodeId::from_parts("a@host", 7),
      }),
      Frame::Data(vec![1, 2, 3]),
      Frame::Ping(42),
      Frame::Pong(42),
      Frame::Close("bye".to_string()),
    ];
    for frame in frames {
      let bytes = encode(&frame).expect("encode");
      assert_eq!(decode(&bytes).expect("decode"), frame);
    }
  }

  #[test]
  fn garbage_fails_cleanly() {
    assert!(decode(&[0xff, 0xff, 0xff, 0xff, 0xff]).is_err());
  }

  #[test]
  fn every_handshake_message_round_trips() {
    let node = NodeId::from_parts("b@host", u128::MAX);
    let msgs = vec![
      HandshakeMsg::Challenge {
        version: PROTOCOL_VERSION,
        node: node.clone(),
        nonce: u64::MAX,
      },
      HandshakeMsg::ChallengeReply {
        digest: [7; 32],
        nonce: 0,
      },
      HandshakeMsg::Welcome { digest: [0xab; 32] },
      HandshakeMsg::Rejected {
        reason: "bad cookie".to_string(),
      },
    ];
    for msg in msgs {
      let frame = Frame::Handshake(msg);
      assert_eq!(
        decode(&encode(&frame).expect("encode")).expect("decode"),
        frame
      );
    }
  }

  #[test]
  fn empty_input_is_an_error() {
    assert!(decode(&[]).is_err());
  }

  #[test]
  fn truncated_frame_is_an_error() {
    let bytes = encode(&Frame::Data(vec![1, 2, 3, 4, 5, 6, 7, 8])).expect("encode");
    assert!(decode(&bytes[..bytes.len() - 1]).is_err());
  }

  #[test]
  fn empty_data_payload_round_trips() {
    let frame = Frame::Data(Vec::new());
    assert_eq!(
      decode(&encode(&frame).expect("encode")).expect("decode"),
      frame
    );
  }

  #[test]
  fn wire_error_display_names_the_codec() {
    let err = decode(&[]).expect_err("empty input");
    assert!(err.to_string().starts_with("wire codec error: "), "{err}");
  }

  #[test]
  fn protocol_version_is_two() {
    assert_eq!(PROTOCOL_VERSION, 2);
  }

  fn arb_frame() -> impl proptest::strategy::Strategy<Value = Frame> {
    use proptest::prelude::*;
    prop_oneof![
      proptest::collection::vec(any::<u8>(), 0..256).prop_map(Frame::Data),
      any::<u64>().prop_map(Frame::Ping),
      any::<u64>().prop_map(Frame::Pong),
      ".{0,64}".prop_map(Frame::Close),
      (any::<u16>(), ".{0,32}", any::<u128>()).prop_map(|(version, name, inc)| {
        Frame::Handshake(HandshakeMsg::Hello {
          version,
          node: NodeId::from_parts(name, inc),
        })
      }),
      (any::<[u8; 32]>(), any::<u64>()).prop_map(|(digest, nonce)| {
        Frame::Handshake(HandshakeMsg::ChallengeReply { digest, nonce })
      }),
    ]
  }

  proptest::proptest! {
    #[test]
    fn any_frame_round_trips(frame in arb_frame()) {
      let bytes = encode(&frame).expect("encode");
      proptest::prop_assert_eq!(decode(&bytes).expect("decode"), frame);
    }

    #[test]
    fn arbitrary_bytes_never_panic(bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..128)) {
      let _ = decode(&bytes);
    }
  }
}
