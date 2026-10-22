//! [`Session`] — an authenticated, framed connection to one peer node.
//!
//! Combines transport connect/accept with the cookie handshake and frame
//! codec. This is the unit the membership and remote-process layers build
//! on: one session per (local, remote) pair per connection epoch.
//!
//! A [`crate::session_pump::SessionPump`] runs a session for a [`crate::Mesh`]: it verifies
//! every envelope the peer sends against [`Session::peer`] before the node's ingress sees it.

use crate::handshake::{Cookie, HandshakeError, initiate, respond};
use crate::id::NodeId;
use crate::transport::{Connection, TransportError};
use crate::wire::{Frame, WireError, decode, encode};

/// Session-level failure.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
  /// Transport connect/IO failure.
  #[error("transport: {0}")]
  Transport(#[from] TransportError),
  /// Handshake failed (bad cookie, version skew, protocol violation).
  #[error("handshake: {0}")]
  Handshake(#[from] HandshakeError),
  /// Frame codec failure — treat as protocol violation and close.
  #[error("wire: {0}")]
  Wire(#[from] WireError),
}

/// An authenticated connection to one peer.
pub struct Session {
  peer: NodeId,
  conn: Box<dyn Connection>,
}

impl Session {
  /// Dial-side: run the initiator handshake over an established
  /// connection.
  pub async fn initiate(
    conn: Box<dyn Connection>,
    me: &NodeId,
    cookie: &Cookie,
  ) -> Result<Self, SessionError> {
    let peer = initiate(conn.as_ref(), me, cookie).await?;
    Ok(Self { peer, conn })
  }

  /// Accept-side: run the responder handshake over an accepted
  /// connection.
  pub async fn respond(
    conn: Box<dyn Connection>,
    me: &NodeId,
    cookie: &Cookie,
  ) -> Result<Self, SessionError> {
    let peer = respond(conn.as_ref(), me, cookie).await?;
    Ok(Self { peer, conn })
  }

  /// The authenticated peer identity.
  pub fn peer(&self) -> &NodeId {
    &self.peer
  }

  /// Send one frame.
  pub async fn send(&self, frame: &Frame) -> Result<(), SessionError> {
    let bytes = encode(frame)?;
    self.conn.send(bytes).await?;
    Ok(())
  }

  /// Receive the next frame.
  pub async fn recv(&self) -> Result<Frame, SessionError> {
    let bytes = self.conn.recv().await?;
    Ok(decode(&bytes)?)
  }

  /// Close the underlying connection.
  pub async fn close(&self) {
    self.conn.close().await;
  }
}

#[cfg(test)]
mod tests {
  use bytes::Bytes;
  use id_effect_process::NodeName;

  use super::*;
  use crate::transport::Transport as _;
  use crate::transport::memory::MemoryNetwork;

  fn cookie(secret: &str) -> Cookie {
    Cookie::new(secret)
  }

  /// `a` dials `b` over a fresh in-memory network; the two handshakes' results.
  async fn pair(
    a_cookie: &Cookie,
    b_cookie: &Cookie,
  ) -> (Result<Session, SessionError>, Result<Session, SessionError>) {
    let (a, b) = (
      NodeId::fresh(&NodeName::new("a@s")),
      NodeId::fresh(&NodeName::new("b@s")),
    );
    let net = MemoryNetwork::new();
    let listener = net.transport("b@s").listen("b@s").await.expect("bind");
    let dial = net.transport("a@s").connect("b@s").await.expect("dial");
    let accepted = listener.accept().await.expect("accept");
    tokio::join!(
      Session::initiate(dial, &a, a_cookie),
      Session::respond(accepted, &b, b_cookie),
    )
  }

  #[tokio::test]
  async fn sessions_exchange_frames_and_fail_typed() {
    let secret = cookie("session-unit-cookie-of-at-least-32-bytes");
    let (a, b) = pair(&secret, &secret).await;
    let (a, b) = (a.expect("a"), b.expect("b"));
    assert_eq!((a.peer().name(), b.peer().name()), ("b@s", "a@s"));
    a.send(&Frame::Ping(7)).await.expect("send");
    assert_eq!(b.recv().await.expect("recv"), Frame::Ping(7));

    // Bytes that are no frame: a wire error.
    a.conn
      .send(Bytes::from_static(&[0xff, 0xff, 0xff]))
      .await
      .expect("raw send");
    let wire = b.recv().await.expect_err("garbage");
    assert!(matches!(wire, SessionError::Wire(_)), "{wire}");
    assert!(wire.to_string().starts_with("wire:"));

    // A closed connection: a transport error, on both ends.
    a.close().await;
    let closed = b.recv().await.expect_err("closed");
    assert!(matches!(closed, SessionError::Transport(_)), "{closed}");
    assert!(closed.to_string().starts_with("transport:"));
    let _ = a.send(&Frame::Ping(8)).await;
  }

  #[tokio::test]
  async fn a_wrong_cookie_fails_the_handshake() {
    let (a, b) = pair(
      &cookie("session-unit-cookie-of-at-least-32-bytes"),
      &cookie("another-cookie-entirely-of-32-bytes!!"),
    )
    .await;
    let failed = a.err().or(b.err()).expect("a side refuses");
    assert!(matches!(failed, SessionError::Handshake(_)), "{failed}");
    assert!(failed.to_string().starts_with("handshake:"));
  }
}
