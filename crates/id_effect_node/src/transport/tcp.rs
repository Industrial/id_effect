//! [`TcpTransport`] — length-delimited frames over Tokio TCP.
//!
//! Frame layout: 4-byte big-endian length prefix + payload. Max frame
//! size is enforced on read so a corrupt/hostile peer cannot force an
//! unbounded allocation.
//!
//! **Plaintext.** TCP frames cross the network unencrypted, and nothing at
//! the transport level authenticates the peer: the cookie handshake proves
//! cluster membership, but an on-path attacker can read every frame and
//! relay or inject traffic. The first dial or bind logs a warning saying
//! so. Use the QUIC transport (feature `quic`) or a trusted network.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Once};

use bytes::Bytes;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener as TokioTcpListener, TcpStream};
use tokio::sync::Mutex;

use super::{Connection, Listener, Transport, TransportError};

/// Hard cap on a single frame (16 MiB). Envelopes larger than this are a
/// protocol violation — bulk data belongs in streams/blob stores.
pub const MAX_FRAME_BYTES: u32 = 16 * 1024 * 1024;

static PLAINTEXT_WARNING: Once = Once::new();

/// Log, once per process, that this transport neither encrypts nor
/// authenticates at the transport level.
fn warn_plaintext() {
  PLAINTEXT_WARNING.call_once(|| {
    tracing::warn!(
      "TcpTransport is unencrypted and unauthenticated at the transport level: \
       frames cross the network in plaintext and an on-path attacker can read, \
       relay or inject them; use the QUIC transport or a trusted network"
    );
  });
}

struct TcpConnection {
  read: Arc<Mutex<OwnedReadHalf>>,
  write: Arc<Mutex<OwnedWriteHalf>>,
  peer: String,
  /// Largest frame `recv` accepts (see `Connection::set_max_frame_bytes`).
  max_frame: AtomicU32,
}

impl TcpConnection {
  fn new(stream: TcpStream) -> Self {
    let peer = stream
      .peer_addr()
      .map(|a| a.to_string())
      .unwrap_or_else(|_| "unknown".to_string());
    let (read, write) = stream.into_split();
    Self {
      read: Arc::new(Mutex::new(read)),
      write: Arc::new(Mutex::new(write)),
      peer,
      max_frame: AtomicU32::new(MAX_FRAME_BYTES),
    }
  }
}

#[async_trait::async_trait]
impl Connection for TcpConnection {
  async fn send(&self, frame: Bytes) -> Result<(), TransportError> {
    if frame.len() as u64 > MAX_FRAME_BYTES as u64 {
      return Err(TransportError::Io(
        "frame exceeds MAX_FRAME_BYTES".to_string(),
      ));
    }
    let mut write = self.write.lock().await;
    write.write_all(&(frame.len() as u32).to_be_bytes()).await?;
    write.write_all(&frame).await?;
    write.flush().await?;
    Ok(())
  }

  async fn recv(&self) -> Result<Bytes, TransportError> {
    let mut read = self.read.lock().await;
    let mut len_buf = [0u8; 4];
    read
      .read_exact(&mut len_buf)
      .await
      .map_err(|_| TransportError::Closed)?;
    let len = u32::from_be_bytes(len_buf);
    frame_within_limit(len, self.max_frame.load(Ordering::Acquire))?;
    let mut payload = vec![0u8; len as usize];
    read
      .read_exact(&mut payload)
      .await
      .map_err(|_| TransportError::Closed)?;
    Ok(Bytes::from(payload))
  }

  async fn close(&self) {
    let mut write = self.write.lock().await;
    let _ = write.shutdown().await;
  }

  fn peer_addr(&self) -> String {
    self.peer.clone()
  }

  fn set_max_frame_bytes(&self, max: u32) {
    self
      .max_frame
      .store(max.min(MAX_FRAME_BYTES), Ordering::Release);
  }
}

/// Refuse a peer's length prefix over `limit` before allocating for it.
pub(crate) fn frame_within_limit(len: u32, limit: u32) -> Result<(), TransportError> {
  if len <= limit {
    Ok(())
  } else if limit >= MAX_FRAME_BYTES {
    Err(TransportError::Io(
      "peer frame exceeds MAX_FRAME_BYTES".to_string(),
    ))
  } else {
    Err(TransportError::Io(format!(
      "peer frame of {len} bytes exceeds the {limit}-byte limit before authentication"
    )))
  }
}

struct TcpListenerWrap {
  listener: TokioTcpListener,
  addr: String,
}

#[async_trait::async_trait]
impl Listener for TcpListenerWrap {
  async fn accept(&self) -> Result<Box<dyn Connection>, TransportError> {
    let (stream, _) = self.listener.accept().await?;
    stream.set_nodelay(true).ok();
    Ok(Box::new(TcpConnection::new(stream)))
  }

  fn local_addr(&self) -> String {
    self.addr.clone()
  }
}

/// Plain-TCP transport (length-delimited frames). The fallback path when
/// QUIC is unavailable; combine with the cookie handshake for auth.
#[derive(Clone, Copy, Debug, Default)]
pub struct TcpTransport;

#[async_trait::async_trait]
impl Transport for TcpTransport {
  async fn connect(&self, addr: &str) -> Result<Box<dyn Connection>, TransportError> {
    warn_plaintext();
    let stream = TcpStream::connect(addr)
      .await
      .map_err(|e| TransportError::Connect(e.to_string()))?;
    stream.set_nodelay(true).ok();
    Ok(Box::new(TcpConnection::new(stream)))
  }

  async fn listen(&self, addr: &str) -> Result<Box<dyn Listener>, TransportError> {
    warn_plaintext();
    let listener = TokioTcpListener::bind(addr)
      .await
      .map_err(|e| TransportError::Bind(e.to_string()))?;
    let local = listener
      .local_addr()
      .map(|a| a.to_string())
      .unwrap_or_else(|_| addr.to_string());
    Ok(Box::new(TcpListenerWrap {
      listener,
      addr: local,
    }))
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  async fn connected() -> (Box<dyn Connection>, Box<dyn Connection>) {
    let listener = TcpTransport.listen("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr();
    let (dial, accepted) = tokio::join!(TcpTransport.connect(&addr), listener.accept());
    (dial.expect("dial"), accepted.expect("accept"))
  }

  #[tokio::test]
  async fn listener_reports_the_resolved_ephemeral_port() {
    let listener = TcpTransport.listen("127.0.0.1:0").await.expect("bind");
    let addr: std::net::SocketAddr = listener.local_addr().parse().expect("socket addr");
    assert_ne!(addr.port(), 0);
  }

  #[tokio::test]
  async fn frames_arrive_in_order_both_ways() {
    let (dial, accepted) = connected().await;
    for i in 0..50u32 {
      dial
        .send(Bytes::from(i.to_be_bytes().to_vec()))
        .await
        .expect("send");
    }
    for i in 0..50u32 {
      assert_eq!(
        accepted.recv().await.expect("recv"),
        Bytes::from(i.to_be_bytes().to_vec())
      );
    }
    accepted
      .send(Bytes::from_static(b"back"))
      .await
      .expect("send");
    assert_eq!(
      dial.recv().await.expect("recv"),
      Bytes::from_static(b"back")
    );
  }

  #[tokio::test]
  async fn empty_frames_are_preserved() {
    let (dial, accepted) = connected().await;
    dial.send(Bytes::new()).await.expect("send");
    dial.send(Bytes::from_static(b"x")).await.expect("send");
    assert!(accepted.recv().await.expect("recv").is_empty());
    assert_eq!(
      accepted.recv().await.expect("recv"),
      Bytes::from_static(b"x")
    );
  }

  #[tokio::test]
  async fn large_frames_cross_intact() {
    let (dial, accepted) = connected().await;
    let payload: Vec<u8> = (0..1_000_000u32).map(|i| i as u8).collect();
    let sent = Bytes::from(payload);
    let (send, recv) = tokio::join!(dial.send(sent.clone()), accepted.recv());
    send.expect("send");
    assert_eq!(recv.expect("recv"), sent);
  }

  #[tokio::test]
  async fn oversized_frames_are_refused_before_writing() {
    let (dial, _accepted) = connected().await;
    let huge = Bytes::from(vec![0u8; MAX_FRAME_BYTES as usize + 1]);
    assert!(matches!(dial.send(huge).await, Err(TransportError::Io(_))));
  }

  #[tokio::test]
  async fn hostile_length_prefix_is_rejected_without_allocating() {
    let listener = TcpTransport.listen("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr();
    let (raw, accepted) = tokio::join!(TcpStream::connect(&addr), listener.accept());
    let mut raw = raw.expect("raw dial");
    let accepted = accepted.expect("accept");
    raw
      .write_all(&u32::MAX.to_be_bytes())
      .await
      .expect("write prefix");
    assert!(matches!(
      accepted.recv().await,
      Err(TransportError::Io(m)) if m.contains("MAX_FRAME_BYTES")
    ));
  }

  #[tokio::test]
  async fn lowered_frame_limit_is_enforced_and_can_be_lifted() {
    let (dial, accepted) = connected().await;
    accepted.set_max_frame_bytes(8);
    dial
      .send(Bytes::from_static(b"12345678"))
      .await
      .expect("send");
    assert_eq!(
      accepted.recv().await.expect("at the limit"),
      Bytes::from_static(b"12345678")
    );
    dial
      .send(Bytes::from_static(b"123456789"))
      .await
      .expect("send");
    assert!(matches!(
      accepted.recv().await,
      Err(TransportError::Io(m)) if m.contains("9 bytes exceeds the 8-byte limit")
    ));
    let (dial, accepted) = connected().await;
    accepted.set_max_frame_bytes(8);
    accepted.set_max_frame_bytes(u32::MAX);
    let big = Bytes::from(vec![7u8; 64 * 1024]);
    let (sent, got) = tokio::join!(dial.send(big.clone()), accepted.recv());
    sent.expect("send");
    assert_eq!(got.expect("lifted, clamped to MAX_FRAME_BYTES"), big);
  }

  #[test]
  fn frame_limit_messages_name_the_limit() {
    assert!(frame_within_limit(MAX_FRAME_BYTES, MAX_FRAME_BYTES).is_ok());
    assert!(matches!(
      frame_within_limit(MAX_FRAME_BYTES + 1, MAX_FRAME_BYTES),
      Err(TransportError::Io(m)) if m.contains("MAX_FRAME_BYTES")
    ));
  }

  #[tokio::test]
  async fn close_is_observed_as_closed_by_the_peer() {
    let (dial, accepted) = connected().await;
    dial.close().await;
    assert!(matches!(accepted.recv().await, Err(TransportError::Closed)));
  }

  #[tokio::test]
  async fn truncated_frame_is_closed_not_partial() {
    let listener = TcpTransport.listen("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr();
    let (raw, accepted) = tokio::join!(TcpStream::connect(&addr), listener.accept());
    let mut raw = raw.expect("raw dial");
    let accepted = accepted.expect("accept");
    raw.write_all(&10u32.to_be_bytes()).await.expect("prefix");
    raw.write_all(b"abc").await.expect("partial body");
    drop(raw);
    assert!(matches!(accepted.recv().await, Err(TransportError::Closed)));
  }

  #[tokio::test]
  async fn peer_addr_is_the_remote_socket() {
    let (dial, accepted) = connected().await;
    assert!(dial.peer_addr().starts_with("127.0.0.1:"));
    assert!(accepted.peer_addr().starts_with("127.0.0.1:"));
    assert_ne!(dial.peer_addr(), accepted.peer_addr());
  }

  #[tokio::test]
  async fn bad_bind_address_is_a_bind_error() {
    let err = TcpTransport
      .listen("not-an-address")
      .await
      .err()
      .expect("bad addr");
    assert!(matches!(err, TransportError::Bind(_)));
  }

  #[tokio::test]
  async fn dial_to_a_closed_port_is_a_connect_error() {
    let listener = TcpTransport.listen("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr();
    drop(listener);
    let err = TcpTransport.connect(&addr).await.err().expect("refused");
    assert!(matches!(err, TransportError::Connect(_)));
  }

  #[test]
  fn io_errors_convert_to_transport_io() {
    let err: TransportError = std::io::Error::other("disk on fire").into();
    assert!(matches!(&err, TransportError::Io(m) if m == "disk on fire"));
    assert_eq!(err.to_string(), "io: disk on fire");
    assert_eq!(TransportError::Closed.to_string(), "connection closed");
    assert_eq!(
      TransportError::Connect("x".into()).to_string(),
      "connect failed: x"
    );
    assert_eq!(
      TransportError::Bind("y".into()).to_string(),
      "bind failed: y"
    );
  }
}
