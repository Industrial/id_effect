//! [`TcpTransport`] — length-delimited frames over Tokio TCP.
//!
//! Frame layout: 4-byte big-endian length prefix + payload. Max frame
//! size is enforced on read so a corrupt/hostile peer cannot force an
//! unbounded allocation.
//!
//! **Encrypted by default.** `TcpTransport::tls` (feature `tls-tcp`)
//! carries the frames in TLS 1.3 with mutual authentication — the trust
//! model of the QUIC transport, configured by `tls_tcp::TlsTcpConfig` from
//! the same certificate material (`transport::certs`). There is no other
//! way to build a [`TcpTransport`] unless the feature
//! `insecure-plaintext-tcp` is enabled.
//!
//! **Plaintext opt-out.** `TcpTransport::dangerous_plaintext` (feature
//! `insecure-plaintext-tcp`) sends frames unencrypted, and nothing at the
//! transport level authenticates the peer: the cookie handshake proves
//! cluster membership, but an on-path attacker can read every frame and
//! relay or inject traffic. The constructor records the opt-out in the
//! posture registry ([`DANGEROUS_PLAINTEXT`], ADR 0013 R8), and the first
//! plaintext dial or bind logs a warning.

use std::sync::atomic::{AtomicU32, Ordering};

use bytes::Bytes;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::Mutex;

#[cfg(feature = "tls-tcp")]
use super::tls_tcp::{self, TlsTcpConfig, TlsTcpConfigError};
use super::{Connection, TransportError};
#[cfg(any(feature = "tls-tcp", feature = "insecure-plaintext-tcp"))]
use super::{Listener, Transport};

/// Hard cap on a single frame (16 MiB). Envelopes larger than this are a
/// protocol violation — bulk data belongs in streams/blob stores.
pub const MAX_FRAME_BYTES: u32 = 16 * 1024 * 1024;

/// Posture id of `TcpTransport::dangerous_plaintext` (feature
/// `insecure-plaintext-tcp`).
pub const DANGEROUS_PLAINTEXT: &str = "id_effect_node::TcpTransport::dangerous_plaintext";

/// Length-delimited frames over a byte stream's read and write halves: a
/// plain TCP socket or a TLS session on one.
pub(crate) struct FramedConnection<R, W> {
  read: Mutex<R>,
  write: Mutex<W>,
  peer: String,
  /// Largest frame `recv` accepts (see `Connection::set_max_frame_bytes`).
  max_frame: AtomicU32,
  /// The TLS exporter secret, on a TLS session.
  binding: Option<[u8; 32]>,
}

impl<R, W> FramedConnection<R, W> {
  #[cfg_attr(
    not(any(feature = "tls-tcp", feature = "insecure-plaintext-tcp")),
    allow(dead_code)
  )]
  pub(crate) fn new(read: R, write: W, peer: String, binding: Option<[u8; 32]>) -> Self {
    Self {
      read: Mutex::new(read),
      write: Mutex::new(write),
      peer,
      max_frame: AtomicU32::new(MAX_FRAME_BYTES),
      binding,
    }
  }
}

#[async_trait::async_trait]
impl<R, W> Connection for FramedConnection<R, W>
where
  R: AsyncRead + Unpin + Send + 'static,
  W: AsyncWrite + Unpin + Send + 'static,
{
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

  fn channel_binding(&self) -> Option<[u8; 32]> {
    self.binding
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

/// TCP transport (length-delimited frames): TLS 1.3 with mutual
/// authentication (`TcpTransport::tls`, feature `tls-tcp`), or plaintext
/// behind the named opt-out `TcpTransport::dangerous_plaintext` (feature
/// `insecure-plaintext-tcp`). Without either feature it cannot be built.
#[derive(Clone)]
pub struct TcpTransport {
  mode: Mode,
}

#[derive(Clone)]
enum Mode {
  #[cfg(feature = "tls-tcp")]
  Tls(std::sync::Arc<TlsTcpConfig>),
  #[cfg(feature = "insecure-plaintext-tcp")]
  Plaintext,
}

impl std::fmt::Debug for TcpTransport {
  #[cfg_attr(
    not(any(feature = "tls-tcp", feature = "insecure-plaintext-tcp")),
    allow(unused_variables)
  )]
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self.mode {
      #[cfg(feature = "tls-tcp")]
      Mode::Tls(ref config) => f.debug_tuple("TcpTransport::Tls").field(config).finish(),
      #[cfg(feature = "insecure-plaintext-tcp")]
      Mode::Plaintext => f.write_str("TcpTransport::Plaintext"),
    }
  }
}

impl TcpTransport {
  /// TLS 1.3 with mutual authentication, without checking `config` (see
  /// [`tls_tcp`] for the trust model). A config with no server trust can
  /// still listen, but every dial fails with [`TransportError::Connect`]
  /// naming [`TlsTcpConfigError::NoServerTrust`]; without client trust,
  /// [`Transport::listen`] fails with [`TransportError::Bind`] naming
  /// [`TlsTcpConfigError::NoClientTrust`]. Prefer [`TcpTransport::try_tls`].
  #[cfg(feature = "tls-tcp")]
  pub fn tls(config: TlsTcpConfig) -> Self {
    Self {
      mode: Mode::Tls(std::sync::Arc::new(config)),
    }
  }

  /// [`TcpTransport::tls`], refusing a config that gives the dialer no way
  /// to authenticate servers. Client trust is checked when the transport
  /// listens (a dial-only transport needs none).
  #[cfg(feature = "tls-tcp")]
  pub fn try_tls(config: TlsTcpConfig) -> Result<Self, TlsTcpConfigError> {
    config.check_trust()?;
    Ok(Self::tls(config))
  }

  /// **Dangerous.** Plaintext TCP: frames cross the network unencrypted
  /// and nothing at the transport level authenticates the peer. Recorded in
  /// the posture registry as [`DANGEROUS_PLAINTEXT`].
  ///
  /// The cookie handshake still proves cluster membership, but there is no
  /// channel to bind its proof to: an on-path attacker can read every frame
  /// and relay or inject traffic. Only for tests and trusted networks; use
  /// `TcpTransport::tls` (feature `tls-tcp`) or the QUIC transport.
  #[cfg(feature = "insecure-plaintext-tcp")]
  pub fn dangerous_plaintext() -> Self {
    id_effect::observability::posture::record_dangerous(
      DANGEROUS_PLAINTEXT,
      "dist.tls#2: mesh frames cross TCP unencrypted and unauthenticated at the transport \
       level, so an on-path host can read, relay or inject them",
    );
    Self {
      mode: Mode::Plaintext,
    }
  }

  /// Whether this transport encrypts and authenticates (TLS), as opposed to
  /// the plaintext opt-out.
  pub fn is_encrypted(&self) -> bool {
    match self.mode {
      #[cfg(feature = "tls-tcp")]
      Mode::Tls(_) => true,
      #[cfg(feature = "insecure-plaintext-tcp")]
      Mode::Plaintext => false,
    }
  }
}

#[cfg(any(feature = "tls-tcp", feature = "insecure-plaintext-tcp"))]
#[async_trait::async_trait]
impl Transport for TcpTransport {
  async fn connect(&self, addr: &str) -> Result<Box<dyn Connection>, TransportError> {
    match self.mode {
      #[cfg(feature = "tls-tcp")]
      Mode::Tls(ref config) => tls_tcp::connect(config, addr).await,
      #[cfg(feature = "insecure-plaintext-tcp")]
      Mode::Plaintext => plaintext::connect(addr).await,
    }
  }

  async fn listen(&self, addr: &str) -> Result<Box<dyn Listener>, TransportError> {
    match self.mode {
      #[cfg(feature = "tls-tcp")]
      Mode::Tls(ref config) => tls_tcp::listen(config, addr).await,
      #[cfg(feature = "insecure-plaintext-tcp")]
      Mode::Plaintext => plaintext::listen(addr).await,
    }
  }
}

/// The `dangerous_plaintext` mode: frames straight on the socket.
#[cfg(feature = "insecure-plaintext-tcp")]
mod plaintext {
  use std::sync::Once;

  use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
  use tokio::net::{TcpListener, TcpStream};

  use super::{Connection, FramedConnection, Listener, TransportError};

  static PLAINTEXT_WARNING: Once = Once::new();

  /// Log, once per process, that this transport neither encrypts nor
  /// authenticates at the transport level.
  fn warn_plaintext() {
    PLAINTEXT_WARNING.call_once(|| {
      tracing::warn!(
        "TcpTransport::dangerous_plaintext is unencrypted and unauthenticated at the transport \
         level: frames cross the network in plaintext and an on-path attacker can read, relay \
         or inject them; use TcpTransport::tls or the QUIC transport"
      );
    });
  }

  fn framed(stream: TcpStream) -> FramedConnection<OwnedReadHalf, OwnedWriteHalf> {
    stream.set_nodelay(true).ok();
    let peer = stream
      .peer_addr()
      .map(|a| a.to_string())
      .unwrap_or_else(|_| "unknown".to_string());
    let (read, write) = stream.into_split();
    FramedConnection::new(read, write, peer, None)
  }

  pub(super) async fn connect(addr: &str) -> Result<Box<dyn Connection>, TransportError> {
    warn_plaintext();
    let stream = TcpStream::connect(addr)
      .await
      .map_err(|e| TransportError::Connect(e.to_string()))?;
    Ok(Box::new(framed(stream)))
  }

  pub(super) async fn listen(addr: &str) -> Result<Box<dyn Listener>, TransportError> {
    warn_plaintext();
    let listener = TcpListener::bind(addr)
      .await
      .map_err(|e| TransportError::Bind(e.to_string()))?;
    let local = listener
      .local_addr()
      .map(|a| a.to_string())
      .unwrap_or_else(|_| addr.to_string());
    Ok(Box::new(PlaintextListener {
      listener,
      addr: local,
    }))
  }

  struct PlaintextListener {
    listener: TcpListener,
    addr: String,
  }

  #[async_trait::async_trait]
  impl Listener for PlaintextListener {
    async fn accept(&self) -> Result<Box<dyn Connection>, TransportError> {
      let (stream, _) = self.listener.accept().await?;
      Ok(Box::new(framed(stream)))
    }

    fn local_addr(&self) -> String {
      self.addr.clone()
    }
  }
}

// Framing over a plain socket; the TLS mode's tests are in `tls_tcp`.
#[cfg(all(test, feature = "insecure-plaintext-tcp"))]
mod tests {
  use tokio::net::TcpStream;

  use super::*;

  fn plain() -> &'static TcpTransport {
    static PLAIN: std::sync::LazyLock<TcpTransport> =
      std::sync::LazyLock::new(TcpTransport::dangerous_plaintext);
    &PLAIN
  }

  #[test]
  fn plaintext_is_not_encrypted() {
    assert!(!plain().is_encrypted());
    assert_eq!(format!("{:?}", plain()), "TcpTransport::Plaintext");
  }

  async fn connected() -> (Box<dyn Connection>, Box<dyn Connection>) {
    let listener = plain().listen("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr();
    let (dial, accepted) = tokio::join!(plain().connect(&addr), listener.accept());
    (dial.expect("dial"), accepted.expect("accept"))
  }

  #[tokio::test]
  async fn listener_reports_the_resolved_ephemeral_port() {
    let listener = plain().listen("127.0.0.1:0").await.expect("bind");
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
    let listener = plain().listen("127.0.0.1:0").await.expect("bind");
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
    let listener = plain().listen("127.0.0.1:0").await.expect("bind");
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
    let err = plain()
      .listen("not-an-address")
      .await
      .err()
      .expect("bad addr");
    assert!(matches!(err, TransportError::Bind(_)));
  }

  #[tokio::test]
  async fn dial_to_a_closed_port_is_a_connect_error() {
    let listener = plain().listen("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr();
    drop(listener);
    let err = plain().connect(&addr).await.err().expect("refused");
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
