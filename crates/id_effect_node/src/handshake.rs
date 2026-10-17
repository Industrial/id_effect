//! Cookie-authenticated mutual handshake (wire protocol v1).
//!
//! BEAM's shared-secret cookie scheme with a modern MAC: both sides prove
//! knowledge of the cookie by HMAC-SHA256 over the peer's nonce and their
//! own node name. The cookie never travels on the wire. This authenticates
//! but does **not** encrypt — pair with the QUIC transport (TLS 1.3) or a
//! trusted network for confidentiality (ADR 0009 § security). The QUIC
//! transport is not mutual TLS: there is no client certificate, and the
//! server certificate is only verified when roots are pinned with
//! `QuicConfig::with_client_verification` (see `transport::quic`, "Trust model").

use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::id::NodeId;
use crate::transport::{Connection, TransportError};
use crate::wire::{Frame, HandshakeMsg, PROTOCOL_VERSION, decode, encode};

type HmacSha256 = Hmac<Sha256>;

/// Shared cluster secret. Compared only through MACs, never sent raw.
#[derive(Clone)]
pub struct Cookie(Vec<u8>);

impl Cookie {
  /// Cookie from any secret bytes/string.
  pub fn new(secret: impl Into<Vec<u8>>) -> Self {
    Self(secret.into())
  }

  fn digest(&self, nonce: u64, peer_name: &str) -> [u8; 32] {
    let mut mac = HmacSha256::new_from_slice(&self.0).expect("HMAC accepts any key length");
    mac.update(&nonce.to_be_bytes());
    mac.update(peer_name.as_bytes());
    mac.finalize().into_bytes().into()
  }

  fn verify(&self, nonce: u64, peer_name: &str, digest: &[u8; 32]) -> bool {
    let mut mac = HmacSha256::new_from_slice(&self.0).expect("HMAC accepts any key length");
    mac.update(&nonce.to_be_bytes());
    mac.update(peer_name.as_bytes());
    mac.verify_slice(digest).is_ok()
  }
}

/// Why a handshake failed.
#[derive(Debug, thiserror::Error)]
pub enum HandshakeError {
  /// Transport died mid-handshake.
  #[error("transport: {0}")]
  Transport(#[from] TransportError),
  /// Peer sent something that is not a valid handshake frame.
  #[error("protocol violation: {0}")]
  Protocol(String),
  /// Peer speaks a different protocol version.
  #[error("version mismatch: ours {ours}, theirs {theirs}")]
  VersionMismatch {
    /// Our protocol version.
    ours: u16,
    /// Peer's protocol version.
    theirs: u16,
  },
  /// Cookie proof failed — not part of this cluster.
  #[error("bad cookie")]
  BadCookie,
  /// Peer explicitly refused.
  #[error("rejected by peer: {0}")]
  Rejected(String),
}

async fn send_hs(conn: &dyn Connection, msg: HandshakeMsg) -> Result<(), HandshakeError> {
  let bytes =
    encode(&Frame::Handshake(msg)).map_err(|e| HandshakeError::Protocol(format!("encode: {e}")))?;
  conn.send(bytes).await?;
  Ok(())
}

async fn recv_hs(conn: &dyn Connection) -> Result<HandshakeMsg, HandshakeError> {
  let bytes = conn.recv().await?;
  match decode(&bytes).map_err(|e| HandshakeError::Protocol(format!("decode: {e}")))? {
    Frame::Handshake(msg) => Ok(msg),
    other => Err(HandshakeError::Protocol(format!(
      "expected handshake frame, got {other:?}"
    ))),
  }
}

/// Run the initiator side. On success the connection is authenticated and
/// the peer's [`NodeId`] is returned.
pub async fn initiate(
  conn: &dyn Connection,
  me: &NodeId,
  cookie: &Cookie,
) -> Result<NodeId, HandshakeError> {
  send_hs(
    conn,
    HandshakeMsg::Hello {
      version: PROTOCOL_VERSION,
      node: me.clone(),
    },
  )
  .await?;

  let (peer, their_nonce) = match recv_hs(conn).await? {
    HandshakeMsg::Challenge {
      version,
      node,
      nonce,
    } => {
      if version != PROTOCOL_VERSION {
        return Err(HandshakeError::VersionMismatch {
          ours: PROTOCOL_VERSION,
          theirs: version,
        });
      }
      (node, nonce)
    }
    HandshakeMsg::Rejected { reason } => return Err(HandshakeError::Rejected(reason)),
    other => {
      return Err(HandshakeError::Protocol(format!(
        "expected Challenge, got {other:?}"
      )));
    }
  };

  let my_nonce: u64 = rand::random();
  send_hs(
    conn,
    HandshakeMsg::ChallengeReply {
      digest: cookie.digest(their_nonce, me.name()),
      nonce: my_nonce,
    },
  )
  .await?;

  match recv_hs(conn).await? {
    HandshakeMsg::Welcome { digest } => {
      if cookie.verify(my_nonce, peer.name(), &digest) {
        Ok(peer)
      } else {
        Err(HandshakeError::BadCookie)
      }
    }
    HandshakeMsg::Rejected { reason } => Err(HandshakeError::Rejected(reason)),
    other => Err(HandshakeError::Protocol(format!(
      "expected Welcome, got {other:?}"
    ))),
  }
}

/// Run the responder side. On success the connection is authenticated and
/// the peer's [`NodeId`] is returned.
pub async fn respond(
  conn: &dyn Connection,
  me: &NodeId,
  cookie: &Cookie,
) -> Result<NodeId, HandshakeError> {
  let peer = match recv_hs(conn).await? {
    HandshakeMsg::Hello { version, node } => {
      if version != PROTOCOL_VERSION {
        let _ = send_hs(
          conn,
          HandshakeMsg::Rejected {
            reason: format!("version mismatch: have {PROTOCOL_VERSION}, got {version}"),
          },
        )
        .await;
        return Err(HandshakeError::VersionMismatch {
          ours: PROTOCOL_VERSION,
          theirs: version,
        });
      }
      node
    }
    other => {
      return Err(HandshakeError::Protocol(format!(
        "expected Hello, got {other:?}"
      )));
    }
  };

  let my_nonce: u64 = rand::random();
  send_hs(
    conn,
    HandshakeMsg::Challenge {
      version: PROTOCOL_VERSION,
      node: me.clone(),
      nonce: my_nonce,
    },
  )
  .await?;

  match recv_hs(conn).await? {
    HandshakeMsg::ChallengeReply { digest, nonce } => {
      if !cookie.verify(my_nonce, peer.name(), &digest) {
        let _ = send_hs(
          conn,
          HandshakeMsg::Rejected {
            reason: "bad cookie".to_string(),
          },
        )
        .await;
        return Err(HandshakeError::BadCookie);
      }
      send_hs(
        conn,
        HandshakeMsg::Welcome {
          digest: cookie.digest(nonce, me.name()),
        },
      )
      .await?;
      Ok(peer)
    }
    HandshakeMsg::Rejected { reason } => Err(HandshakeError::Rejected(reason)),
    other => Err(HandshakeError::Protocol(format!(
      "expected ChallengeReply, got {other:?}"
    ))),
  }
}

#[cfg(test)]
mod tests {
  use bytes::Bytes;

  use super::*;
  use crate::transport::Transport;
  use crate::transport::memory::MemoryNetwork;

  /// Connected pair: (side under test, raw scripted peer).
  async fn pair() -> (Box<dyn Connection>, Box<dyn Connection>) {
    let net = MemoryNetwork::new();
    let listener = net.transport("b").listen("b").await.expect("bind");
    let dial = net.transport("a").connect("b").await.expect("dial");
    let accepted = listener.accept().await.expect("accept");
    (dial, accepted)
  }

  fn me() -> NodeId {
    NodeId::from_parts("a@test", 1)
  }

  fn peer() -> NodeId {
    NodeId::from_parts("b@test", 2)
  }

  async fn run_initiate(conn: &dyn Connection) -> Result<NodeId, HandshakeError> {
    initiate(conn, &me(), &Cookie::new("c")).await
  }

  async fn run_respond(conn: &dyn Connection) -> Result<NodeId, HandshakeError> {
    respond(conn, &me(), &Cookie::new("c")).await
  }

  async fn send_raw(conn: &dyn Connection, msg: HandshakeMsg) {
    send_hs(conn, msg).await.expect("raw send");
  }

  #[test]
  fn digest_verifies_under_the_same_cookie() {
    let cookie = Cookie::new("secret");
    let digest = cookie.digest(9, "a@test");
    assert!(cookie.verify(9, "a@test", &digest));
  }

  #[test]
  fn digest_fails_under_a_different_cookie() {
    let digest = Cookie::new("secret").digest(9, "a@test");
    assert!(!Cookie::new("other").verify(9, "a@test", &digest));
  }

  #[test]
  fn digest_is_bound_to_nonce_and_name() {
    let cookie = Cookie::new("secret");
    let digest = cookie.digest(9, "a@test");
    assert!(!cookie.verify(10, "a@test", &digest), "replayed nonce");
    assert!(!cookie.verify(9, "b@test", &digest), "other node's proof");
  }

  #[test]
  fn empty_cookie_still_macs() {
    let cookie = Cookie::new(Vec::<u8>::new());
    let digest = cookie.digest(1, "a@test");
    assert!(cookie.verify(1, "a@test", &digest));
  }

  #[test]
  fn error_display_is_human_readable() {
    assert_eq!(HandshakeError::BadCookie.to_string(), "bad cookie");
    assert_eq!(
      HandshakeError::VersionMismatch { ours: 1, theirs: 2 }.to_string(),
      "version mismatch: ours 1, theirs 2"
    );
    assert_eq!(
      HandshakeError::Rejected("nope".into()).to_string(),
      "rejected by peer: nope"
    );
    assert_eq!(
      HandshakeError::Protocol("x".into()).to_string(),
      "protocol violation: x"
    );
    let from: HandshakeError = TransportError::Closed.into();
    assert_eq!(from.to_string(), "transport: connection closed");
  }

  #[tokio::test]
  async fn responder_rejects_version_skew_and_tells_the_peer() {
    let (raw, conn) = pair().await;
    send_raw(
      raw.as_ref(),
      HandshakeMsg::Hello {
        version: PROTOCOL_VERSION + 1,
        node: peer(),
      },
    )
    .await;
    let err = run_respond(conn.as_ref()).await.expect_err("skew");
    assert!(matches!(
      err,
      HandshakeError::VersionMismatch { ours: PROTOCOL_VERSION, theirs } if theirs == PROTOCOL_VERSION + 1
    ));
    assert!(matches!(
      recv_hs(raw.as_ref()).await.expect("rejection"),
      HandshakeMsg::Rejected { .. }
    ));
  }

  #[tokio::test]
  async fn responder_refuses_a_non_hello_opening() {
    let (raw, conn) = pair().await;
    send_raw(raw.as_ref(), HandshakeMsg::Welcome { digest: [0; 32] }).await;
    let err = run_respond(conn.as_ref()).await.expect_err("protocol");
    assert!(matches!(err, HandshakeError::Protocol(_)), "{err}");
  }

  #[tokio::test]
  async fn responder_refuses_a_data_frame_before_auth() {
    let (raw, conn) = pair().await;
    raw
      .send(encode(&Frame::Data(vec![1])).expect("encode"))
      .await
      .expect("send");
    let err = run_respond(conn.as_ref()).await.expect_err("protocol");
    assert!(
      matches!(err, HandshakeError::Protocol(msg) if msg.contains("expected handshake frame"))
    );
  }

  #[tokio::test]
  async fn garbage_bytes_are_a_protocol_violation() {
    let (raw, conn) = pair().await;
    raw
      .send(Bytes::from_static(&[0xff, 0xff, 0xff]))
      .await
      .expect("send");
    let err = run_respond(conn.as_ref()).await.expect_err("protocol");
    assert!(matches!(err, HandshakeError::Protocol(msg) if msg.starts_with("decode")));
  }

  #[tokio::test]
  async fn responder_rejects_a_forged_challenge_reply() {
    let (raw, conn) = pair().await;
    let script = async {
      send_raw(
        raw.as_ref(),
        HandshakeMsg::Hello {
          version: PROTOCOL_VERSION,
          node: peer(),
        },
      )
      .await;
      assert!(matches!(
        recv_hs(raw.as_ref()).await.expect("challenge"),
        HandshakeMsg::Challenge { .. }
      ));
      send_raw(
        raw.as_ref(),
        HandshakeMsg::ChallengeReply {
          digest: [0; 32],
          nonce: 1,
        },
      )
      .await;
      recv_hs(raw.as_ref()).await.expect("verdict")
    };
    let (verdict, result) = tokio::join!(script, run_respond(conn.as_ref()));
    assert!(matches!(result, Err(HandshakeError::BadCookie)));
    assert!(matches!(verdict, HandshakeMsg::Rejected { reason } if reason == "bad cookie"));
  }

  #[tokio::test]
  async fn responder_surfaces_an_initiator_rejection() {
    let (raw, conn) = pair().await;
    let script = async {
      send_raw(
        raw.as_ref(),
        HandshakeMsg::Hello {
          version: PROTOCOL_VERSION,
          node: peer(),
        },
      )
      .await;
      let _ = recv_hs(raw.as_ref()).await.expect("challenge");
      send_raw(
        raw.as_ref(),
        HandshakeMsg::Rejected {
          reason: "changed my mind".into(),
        },
      )
      .await;
    };
    let ((), result) = tokio::join!(script, run_respond(conn.as_ref()));
    assert!(matches!(result, Err(HandshakeError::Rejected(r)) if r == "changed my mind"));
  }

  #[tokio::test]
  async fn initiator_surfaces_an_explicit_rejection() {
    let (conn, raw) = pair().await;
    let script = async {
      let _ = recv_hs(raw.as_ref()).await.expect("hello");
      send_raw(
        raw.as_ref(),
        HandshakeMsg::Rejected {
          reason: "cluster full".into(),
        },
      )
      .await;
    };
    let ((), result) = tokio::join!(script, run_initiate(conn.as_ref()));
    assert!(matches!(result, Err(HandshakeError::Rejected(r)) if r == "cluster full"));
  }

  #[tokio::test]
  async fn initiator_rejects_version_skew_in_challenge() {
    let (conn, raw) = pair().await;
    let script = async {
      let _ = recv_hs(raw.as_ref()).await.expect("hello");
      send_raw(
        raw.as_ref(),
        HandshakeMsg::Challenge {
          version: 99,
          node: peer(),
          nonce: 5,
        },
      )
      .await;
    };
    let ((), result) = tokio::join!(script, run_initiate(conn.as_ref()));
    assert!(matches!(
      result,
      Err(HandshakeError::VersionMismatch { theirs: 99, .. })
    ));
  }

  #[tokio::test]
  async fn initiator_refuses_an_unexpected_reply_to_hello() {
    let (conn, raw) = pair().await;
    let script = async {
      let _ = recv_hs(raw.as_ref()).await.expect("hello");
      send_raw(raw.as_ref(), HandshakeMsg::Welcome { digest: [0; 32] }).await;
    };
    let ((), result) = tokio::join!(script, run_initiate(conn.as_ref()));
    assert!(matches!(result, Err(HandshakeError::Protocol(m)) if m.contains("expected Challenge")));
  }

  #[tokio::test]
  async fn initiator_detects_a_responder_without_the_cookie() {
    let (conn, raw) = pair().await;
    let script = async {
      let _ = recv_hs(raw.as_ref()).await.expect("hello");
      send_raw(
        raw.as_ref(),
        HandshakeMsg::Challenge {
          version: PROTOCOL_VERSION,
          node: peer(),
          nonce: 5,
        },
      )
      .await;
      let _ = recv_hs(raw.as_ref()).await.expect("reply");
      // An impostor cannot MAC the initiator's nonce without the cookie.
      send_raw(raw.as_ref(), HandshakeMsg::Welcome { digest: [0; 32] }).await;
    };
    let ((), result) = tokio::join!(script, run_initiate(conn.as_ref()));
    assert!(matches!(result, Err(HandshakeError::BadCookie)));
  }

  #[tokio::test]
  async fn initiator_sends_a_proof_bound_to_its_own_name() {
    let (conn, raw) = pair().await;
    let cookie = Cookie::new("c");
    let script = async {
      let hello = recv_hs(raw.as_ref()).await.expect("hello");
      assert!(
        matches!(hello, HandshakeMsg::Hello { version: PROTOCOL_VERSION, ref node } if *node == me())
      );
      send_raw(
        raw.as_ref(),
        HandshakeMsg::Challenge {
          version: PROTOCOL_VERSION,
          node: peer(),
          nonce: 77,
        },
      )
      .await;
      let HandshakeMsg::ChallengeReply { digest, nonce } =
        recv_hs(raw.as_ref()).await.expect("reply")
      else {
        panic!("expected ChallengeReply");
      };
      assert!(cookie.verify(77, "a@test", &digest));
      send_raw(
        raw.as_ref(),
        HandshakeMsg::Welcome {
          digest: cookie.digest(nonce, "b@test"),
        },
      )
      .await;
    };
    let initiator = me();
    let ((), result) = tokio::join!(script, initiate(conn.as_ref(), &initiator, &cookie));
    assert_eq!(result.expect("authenticated"), peer());
  }

  #[tokio::test]
  async fn closed_transport_is_a_transport_error() {
    let (conn, raw) = pair().await;
    raw.close().await;
    let err = run_initiate(conn.as_ref()).await.expect_err("closed");
    assert!(matches!(
      err,
      HandshakeError::Transport(TransportError::Closed)
    ));
  }
}
