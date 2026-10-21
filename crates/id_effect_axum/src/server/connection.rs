//! The accept loop behind every server entry point ([`serve_listener`], [`serve_host_listener`],
//! and with feature `tls` `serve_tls_listener`): one hyper-util connection per accepted socket, bounded by
//! [`ConnectionLimits`] (header-read timeout, keep-alive limit, connection cap; on TLS a
//! handshake timeout, on HTTP/2 a concurrent-stream cap), with the peer address recorded on
//! every request as `ConnectInfo<SocketAddr>` (so [`ClientIp`](crate::ClientIp) and per-IP rate
//! limits see it).
//!
//! Plain TCP serves HTTP/1.1. TLS (feature `tls`, `HostTransport::tls`) serves the protocol
//! ALPN chose: `h2` → HTTP/2, `http/1.1` or none → HTTP/1.1.
//!
//! Graceful shutdown is axum's: when the shutdown future completes the listener stops accepting,
//! every open connection finishes its in-flight requests and closes (HTTP/2 with a `GOAWAY`),
//! and the loop returns once the last one has.
//!
//! ```rust,no_run
//! use std::time::Duration;
//!
//! use axum::Router;
//! use id_effect_axum::server::{ConnectionLimits, serve_listener};
//!
//! # async fn run() -> std::io::Result<()> {
//! let listener = tokio::net::TcpListener::bind("127.0.0.1:8080").await?;
//! let limits = ConnectionLimits::new()
//!   .header_read_timeout(Duration::from_secs(5))
//!   .max_connections(2_000);
//! serve_listener(listener, Router::new(), limits, async {
//!   let _ = tokio::signal::ctrl_c().await;
//! })
//! .await
//! # }
//! ```

use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::{Pin, pin};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::ConnectInfo;
use axum::http::{HeaderValue, Request, Version, header};
use axum::response::Response;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use hyper_util::server::conn::auto::Builder;
use hyper_util::service::TowerToHyperService;
use id_effect::observability::posture::record_dangerous;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore, watch};
use tower_service::Service;

use crate::server::bootstrap::HostConfig;
#[cfg(feature = "tls")]
use crate::server::tls::{Negotiated, ServerTls};
#[cfg(feature = "http3")]
use id_effect_platform::tls::{CertReloader, Identity, ReloadTask};

/// Posture id of [`ConnectionLimits::dangerous_no_tls_handshake_timeout`] (ADR 0013 R10).
pub const DANGEROUS_NO_TLS_HANDSHAKE_TIMEOUT: &str =
  "id_effect_axum::server::ConnectionLimits::dangerous_no_tls_handshake_timeout";

/// Posture id of [`ConnectionLimits::dangerous_unlimited_concurrent_streams`] (ADR 0013 R10).
pub const DANGEROUS_UNLIMITED_CONCURRENT_STREAMS: &str =
  "id_effect_axum::server::ConnectionLimits::dangerous_unlimited_concurrent_streams";

/// Posture id of [`ConnectionLimits::dangerous_no_header_read_timeout`] (ADR 0013 R10).
pub const DANGEROUS_NO_HEADER_READ_TIMEOUT: &str =
  "id_effect_axum::server::ConnectionLimits::dangerous_no_header_read_timeout";

/// Posture id of [`ConnectionLimits::dangerous_unlimited_connections`] (ADR 0013 R10).
pub const DANGEROUS_UNLIMITED_CONNECTIONS: &str =
  "id_effect_axum::server::ConnectionLimits::dangerous_unlimited_connections";

/// Posture id of [`ConnectionLimits::dangerous_unlimited_requests_per_connection`].
pub const DANGEROUS_UNLIMITED_REQUESTS_PER_CONNECTION: &str =
  "id_effect_axum::server::ConnectionLimits::dangerous_unlimited_requests_per_connection";

/// Per-connection limits of the server (slowloris and connection exhaustion). The defaults are
/// on; each `dangerous_*` method turns one off.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectionLimits {
  header_read_timeout: Option<Duration>,
  max_connections: Option<usize>,
  keep_alive: bool,
  max_requests_per_connection: Option<usize>,
  #[cfg_attr(not(feature = "tls"), allow(dead_code))]
  tls_handshake_timeout: Option<Duration>,
  #[cfg_attr(not(feature = "tls"), allow(dead_code))]
  max_concurrent_streams: Option<u32>,
}

impl Default for ConnectionLimits {
  /// [`Self::DEFAULT_HEADER_READ_TIMEOUT`], [`Self::DEFAULT_MAX_CONNECTIONS`], keep-alive on with
  /// [`Self::DEFAULT_MAX_REQUESTS_PER_CONNECTION`], [`Self::DEFAULT_TLS_HANDSHAKE_TIMEOUT`] and
  /// [`Self::DEFAULT_MAX_CONCURRENT_STREAMS`].
  fn default() -> Self {
    Self {
      header_read_timeout: Some(Self::DEFAULT_HEADER_READ_TIMEOUT),
      max_connections: Some(Self::DEFAULT_MAX_CONNECTIONS),
      keep_alive: true,
      max_requests_per_connection: Some(Self::DEFAULT_MAX_REQUESTS_PER_CONNECTION),
      tls_handshake_timeout: Some(Self::DEFAULT_TLS_HANDSHAKE_TIMEOUT),
      max_concurrent_streams: Some(Self::DEFAULT_MAX_CONCURRENT_STREAMS),
    }
  }
}

impl ConnectionLimits {
  /// Default [`Self::header_read_timeout`]: 10 s.
  pub const DEFAULT_HEADER_READ_TIMEOUT: Duration = Duration::from_secs(10);
  /// Default [`Self::max_connections`]: 10,000.
  pub const DEFAULT_MAX_CONNECTIONS: usize = 10_000;
  /// Default [`Self::max_requests_per_connection`]: 1,000.
  pub const DEFAULT_MAX_REQUESTS_PER_CONNECTION: usize = 1_000;
  /// Default [`Self::tls_handshake_timeout`]: 10 s.
  pub const DEFAULT_TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
  /// Default [`Self::max_concurrent_streams`]: 100 (the floor RFC 9113 §6.5.2 recommends).
  pub const DEFAULT_MAX_CONCURRENT_STREAMS: u32 = 100;

  /// The defaults (see [`Default`]).
  pub fn new() -> Self {
    Self::default()
  }

  /// Close a TLS connection whose handshake has not completed within `timeout` (feature `tls`).
  /// The connection holds its [`Self::max_connections`] slot meanwhile, so a silent client
  /// cannot hold slots forever.
  #[must_use]
  pub fn tls_handshake_timeout(mut self, timeout: Duration) -> Self {
    self.tls_handshake_timeout = Some(timeout);
    self
  }

  /// No TLS handshake timeout: a client may hold a connection (and its slot) open forever by
  /// never finishing the handshake.
  ///
  /// **Re-opens slowloris during the handshake** (finding M1 of the 0.10.1 security review, on
  /// TLS): only behind a proxy that enforces its own timeout. Recorded in the
  /// [posture registry](id_effect::observability::posture) as
  /// [`DANGEROUS_NO_TLS_HANDSHAKE_TIMEOUT`].
  #[must_use]
  pub fn dangerous_no_tls_handshake_timeout(mut self) -> Self {
    record_dangerous(
      DANGEROUS_NO_TLS_HANDSHAKE_TIMEOUT,
      "M1: no TLS handshake timeout; a client that never finishes the handshake holds a \
       connection open forever (slowloris)",
    );
    self.tls_handshake_timeout = None;
    self
  }

  /// Let an HTTP/2 client open at most `n` (at least 1) streams at once on one connection
  /// (`SETTINGS_MAX_CONCURRENT_STREAMS`; feature `tls`). Further requests wait at the client.
  #[must_use]
  pub fn max_concurrent_streams(mut self, n: u32) -> Self {
    self.max_concurrent_streams = Some(n.max(1));
    self
  }

  /// No HTTP/2 concurrent-stream cap: one connection may run any number of requests at once.
  ///
  /// **Re-opens stream exhaustion** (ADR 0014 C3 on HTTP/2): one client multiplies its work
  /// and memory on the server without opening another connection. Recorded in the
  /// [posture registry](id_effect::observability::posture) as
  /// [`DANGEROUS_UNLIMITED_CONCURRENT_STREAMS`].
  #[must_use]
  pub fn dangerous_unlimited_concurrent_streams(mut self) -> Self {
    record_dangerous(
      DANGEROUS_UNLIMITED_CONCURRENT_STREAMS,
      "C3: no HTTP/2 concurrent-stream cap; one connection runs any number of requests at once",
    );
    self.max_concurrent_streams = None;
    self
  }

  /// Close a connection whose client has not sent a complete request head within `timeout`: the
  /// first request after connecting (any protocol), and on HTTP/1 every following one, so an
  /// idle keep-alive connection is closed after `timeout` too.
  #[must_use]
  pub fn header_read_timeout(mut self, timeout: Duration) -> Self {
    self.header_read_timeout = Some(timeout);
    self
  }

  /// No header-read timeout: a client may hold a connection open forever by sending its request
  /// head slowly (or not at all), or by idling on keep-alive.
  ///
  /// **Re-opens slowloris** (finding M1 of the 0.10.1 security review): only for a server
  /// behind a proxy that enforces its own timeout. Recorded in the
  /// [posture registry](id_effect::observability::posture) as
  /// [`DANGEROUS_NO_HEADER_READ_TIMEOUT`].
  #[must_use]
  pub fn dangerous_no_header_read_timeout(mut self) -> Self {
    record_dangerous(
      DANGEROUS_NO_HEADER_READ_TIMEOUT,
      "M1: no header-read timeout; a slow or silent client holds a connection open forever \
       (slowloris)",
    );
    self.header_read_timeout = None;
    self
  }

  /// Serve at most `n` (at least 1) connections at once; further clients wait in the listen
  /// backlog until one closes.
  #[must_use]
  pub fn max_connections(mut self, n: usize) -> Self {
    self.max_connections = Some(n.clamp(1, Semaphore::MAX_PERMITS));
    self
  }

  /// No connection cap: every accepted socket is served at once.
  ///
  /// **Re-opens connection exhaustion** (finding M1 of the 0.10.1 security review): a client
  /// opening sockets in a loop can run the process out of file descriptors and memory.
  /// Recorded in the [posture registry](id_effect::observability::posture) as
  /// [`DANGEROUS_UNLIMITED_CONNECTIONS`].
  #[must_use]
  pub fn dangerous_unlimited_connections(mut self) -> Self {
    record_dangerous(
      DANGEROUS_UNLIMITED_CONNECTIONS,
      "M1: no connection cap; a client opening sockets in a loop exhausts file descriptors and \
       memory",
    );
    self.max_connections = None;
    self
  }

  /// Whether HTTP/1 connections are kept alive between requests (default `true`).
  #[must_use]
  pub fn keep_alive(mut self, enabled: bool) -> Self {
    self.keep_alive = enabled;
    self
  }

  /// Close a connection after `n` (at least 1) requests: on HTTP/1 the `n`th response carries
  /// `Connection: close`; on HTTP/2 the `n`th request starts a graceful shutdown (`GOAWAY`), the
  /// streams already open finish.
  #[must_use]
  pub fn max_requests_per_connection(mut self, n: usize) -> Self {
    self.max_requests_per_connection = Some(n.max(1));
    self
  }

  /// No limit on the requests one connection (HTTP/1 keep-alive or HTTP/2) may make.
  ///
  /// **Loosens the keep-alive limit** (finding M1 of the 0.10.1 security review): one client
  /// may hold a connection, and its slot under [`Self::max_connections`], for as long as it
  /// keeps sending requests. Recorded in the
  /// [posture registry](id_effect::observability::posture) as
  /// [`DANGEROUS_UNLIMITED_REQUESTS_PER_CONNECTION`].
  #[must_use]
  pub fn dangerous_unlimited_requests_per_connection(mut self) -> Self {
    record_dangerous(
      DANGEROUS_UNLIMITED_REQUESTS_PER_CONNECTION,
      "M1: no per-connection request limit; one keep-alive client holds its connection slot for \
       as long as it keeps sending",
    );
    self.max_requests_per_connection = None;
    self
  }

  /// The header-read timeout in force (`None`: off).
  #[cfg_attr(not(feature = "http3"), allow(dead_code))]
  pub(crate) fn header_read_timeout_limit(&self) -> Option<Duration> {
    self.header_read_timeout
  }

  /// The connection cap in force (`None`: unlimited).
  #[cfg_attr(not(feature = "http3"), allow(dead_code))]
  pub(crate) fn connections_limit(&self) -> Option<usize> {
    self.max_connections
  }

  /// The per-connection request limit in force (`None`: unlimited).
  #[cfg_attr(not(feature = "http3"), allow(dead_code))]
  pub(crate) fn requests_per_connection_limit(&self) -> Option<usize> {
    self.max_requests_per_connection
  }

  /// The TLS handshake timeout in force (`None`: off).
  #[cfg_attr(not(feature = "http3"), allow(dead_code))]
  pub(crate) fn tls_handshake_timeout_limit(&self) -> Option<Duration> {
    self.tls_handshake_timeout
  }

  /// The concurrent-stream cap in force (`None`: unlimited).
  #[cfg_attr(not(feature = "http3"), allow(dead_code))]
  pub(crate) fn concurrent_streams_limit(&self) -> Option<u32> {
    self.max_concurrent_streams
  }

  /// The hyper-util builder of one connection speaking `protocol`.
  fn builder(&self, protocol: Protocol) -> Builder<TokioExecutor> {
    let mut builder = Builder::new(TokioExecutor::new());
    builder
      .http1()
      .timer(TokioTimer::new())
      .header_read_timeout(self.header_read_timeout)
      .keep_alive(self.keep_alive);
    #[cfg(feature = "tls")]
    builder
      .http2()
      .timer(TokioTimer::new())
      .max_concurrent_streams(self.max_concurrent_streams)
      // Dead peers: a PING every 20 s, closed when unanswered for 20 s.
      .keep_alive_interval(Some(Duration::from_secs(20)))
      .keep_alive_timeout(Duration::from_secs(20));
    match protocol {
      Protocol::Auto => builder,
      #[cfg(feature = "tls")]
      Protocol::Http1 => builder.http1_only(),
      #[cfg(feature = "tls")]
      Protocol::Http2 => builder.http2_only(),
    }
  }
}

/// The protocol one connection is served with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Protocol {
  /// Plain TCP: whatever the client speaks among the compiled-in protocols (HTTP/1.1).
  Auto,
  /// TLS with ALPN `http/1.1` or none.
  #[cfg(feature = "tls")]
  Http1,
  /// TLS with ALPN `h2`.
  #[cfg(feature = "tls")]
  Http2,
}

/// What a listener speaks: plain HTTP (the default) or, with feature `tls`, TLS on TCP with
/// ALPN `h2` / `http/1.1` ([`HostTransport::from_config`] picks it from [`HostConfig::tls`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostTransport {
  #[cfg(feature = "tls")]
  tls: Option<ServerTls>,
  #[cfg(feature = "http3")]
  http3: Option<Http3Identity>,
}

/// The certificate the HTTP/3 listener presents, and the file polling that keeps it current.
#[cfg(feature = "http3")]
#[derive(Clone, Debug)]
struct Http3Identity {
  identity: Identity,
  _reload: Option<Arc<ReloadTask>>,
}

/// Two are equal when they present the same certificate now.
#[cfg(feature = "http3")]
impl PartialEq for Http3Identity {
  fn eq(&self, other: &Self) -> bool {
    self.identity.cert_sha256() == other.identity.cert_sha256()
  }
}

#[cfg(feature = "http3")]
impl Eq for Http3Identity {}

impl HostTransport {
  /// Plain HTTP on TCP.
  pub fn plain() -> Self {
    Self::default()
  }

  /// TLS on TCP with `tls`.
  #[cfg(feature = "tls")]
  #[allow(clippy::needless_update)] // the update fills `http3` with feature `http3`
  pub fn tls(tls: ServerTls) -> Self {
    Self {
      tls: Some(tls),
      ..Self::default()
    }
  }

  /// The certificate HTTP/3 presents (feature `http3`): the [`Identity`] the TLS listener
  /// presents too, so a reload reaches both. [`HostTransport::from_config`] sets it for
  /// certificate files ([`HostTls::Files`](crate::server::HostTls::Files)); provided TLS
  /// settings present theirs ([`ServerTls::identity`]), and settings without one (a bare
  /// resolver) serve TCP only.
  #[cfg(feature = "http3")]
  pub fn http3_identity(&self) -> Option<&Identity> {
    self
      .http3
      .as_ref()
      .map(|h3| &h3.identity)
      .or_else(|| self.tls.as_ref().and_then(ServerTls::identity))
  }

  /// The TLS settings, when the transport is TLS.
  #[cfg(feature = "tls")]
  pub fn server_tls(&self) -> Option<&ServerTls> {
    self.tls.as_ref()
  }

  /// Whether connections are TLS.
  #[allow(clippy::let_and_return)] // one binding per feature set
  pub fn is_tls(&self) -> bool {
    #[cfg(feature = "tls")]
    let tls = self.tls.is_some();
    #[cfg(not(feature = "tls"))]
    let tls = false;
    tls
  }

  /// The transport [`HostConfig`] asks for: plain without [`HostConfig::tls`], else TLS from
  /// its certificate files (polled for rotation on the current Tokio runtime) or identity.
  /// Logs the config's [posture warnings](HostConfig::posture_warnings): at `warn` in the
  /// `prod` profile (`ID_EFFECT_PROFILE`), at `info` otherwise.
  ///
  /// # Errors
  ///
  /// [`std::io::ErrorKind::InvalidInput`] when the certificate cannot be loaded;
  /// [`std::io::ErrorKind::Unsupported`] when [`HostConfig::tls`] is set but the crate was built
  /// without feature `tls` (the host refuses to fall back to plain HTTP).
  pub fn from_config(config: &HostConfig) -> std::io::Result<Self> {
    let prod = std::env::var(id_effect_config::PROFILE_ENV_VAR).is_ok_and(|p| p == "prod");
    log_posture(config, prod);
    match &config.tls {
      None => Ok(Self::plain()),
      Some(tls) => tls_transport(tls),
    }
  }
}

/// Logs `config`'s posture warnings: at `warn` in prod, at `info` otherwise.
fn log_posture(config: &HostConfig, prod: bool) -> usize {
  let warnings = config.posture_warnings();
  for warning in &warnings {
    if prod {
      tracing::warn!(target: "id_effect_axum::posture", "{warning}");
    } else {
      tracing::info!(target: "id_effect_axum::posture", "{warning}");
    }
  }
  warnings.len()
}

#[cfg(feature = "tls")]
fn invalid_tls(e: id_effect_platform::tls::CertError) -> std::io::Error {
  std::io::Error::new(std::io::ErrorKind::InvalidInput, e)
}

#[cfg(all(feature = "tls", not(feature = "http3")))]
fn tls_transport(tls: &crate::server::bootstrap::HostTls) -> std::io::Result<HostTransport> {
  tls
    .server_tls()
    .map(HostTransport::tls)
    .map_err(invalid_tls)
}

/// With HTTP/3, certificate files are loaded into one [`Identity`] that both listeners present
/// (and that the file polling swaps for both); provided settings serve TCP only.
#[cfg(feature = "http3")]
fn tls_transport(tls: &crate::server::bootstrap::HostTls) -> std::io::Result<HostTransport> {
  use crate::server::bootstrap::HostTls;
  let HostTls::Files {
    cert_file,
    key_file,
    reload_every,
  } = tls
  else {
    return tls
      .server_tls()
      .map(HostTransport::tls)
      .map_err(invalid_tls);
  };
  let reloader = Arc::new(CertReloader::new(cert_file, key_file).map_err(invalid_tls)?);
  let reload = match reload_every {
    Some(every) => {
      if tokio::runtime::Handle::try_current().is_err() {
        return Err(invalid_tls(id_effect_platform::tls::CertError::Tls(
          "certificate polling needs a Tokio runtime".to_owned(),
        )));
      }
      Some(Arc::new(reloader.spawn_polling(*every)))
    }
    None => None,
  };
  let identity = reloader.identity();
  let server = ServerTls::from_identity(&identity).map_err(invalid_tls)?;
  Ok(HostTransport {
    tls: Some(server),
    http3: Some(Http3Identity {
      identity,
      _reload: reload,
    }),
  })
}

#[cfg(not(feature = "tls"))]
fn tls_transport(_tls: &crate::server::bootstrap::HostTls) -> std::io::Result<HostTransport> {
  Err(std::io::Error::new(
    std::io::ErrorKind::Unsupported,
    "HostConfig::tls is set but id_effect_axum was built without feature `tls`; refusing to \
     serve plain HTTP",
  ))
}

/// Serves `app` on `listener` within `limits` until `shutdown` completes, then shuts down
/// gracefully (see the [module docs](self)). Every request carries `ConnectInfo<SocketAddr>`.
///
/// # Errors
///
/// None so far: accept errors are retried (after a second, unless the peer reset the
/// connection), as `axum::serve` does. The `io::Result` leaves room for listener failures.
pub async fn serve_listener<F>(
  listener: TcpListener,
  app: Router,
  limits: ConnectionLimits,
  shutdown: F,
) -> std::io::Result<()>
where
  F: Future<Output = ()> + Send + 'static,
{
  serve_host_listener(listener, app, limits, HostTransport::plain(), shutdown).await
}

/// [`serve_listener`] over TLS (feature `tls`): each accepted socket completes a TLS handshake
/// within [`ConnectionLimits::tls_handshake_timeout`] first, then speaks the protocol ALPN chose
/// (`h2` or `http/1.1`); a handshake that fails or times out closes the socket.
///
/// # Errors
///
/// As [`serve_listener`].
#[cfg(feature = "tls")]
pub async fn serve_tls_listener<F>(
  listener: TcpListener,
  app: Router,
  limits: ConnectionLimits,
  tls: ServerTls,
  shutdown: F,
) -> std::io::Result<()>
where
  F: Future<Output = ()> + Send + 'static,
{
  serve_host_listener(listener, app, limits, HostTransport::tls(tls), shutdown).await
}

/// [`serve_listener`] speaking `transport` (plain or TLS).
///
/// # Errors
///
/// As [`serve_listener`].
pub async fn serve_host_listener<F>(
  listener: TcpListener,
  app: Router,
  limits: ConnectionLimits,
  transport: HostTransport,
  shutdown: F,
) -> std::io::Result<()>
where
  F: Future<Output = ()> + Send + 'static,
{
  // Dropping the receiver is the signal; connections watch the sender.
  let (signal_tx, signal_rx) = watch::channel(());
  tokio::spawn(async move {
    shutdown.await;
    drop(signal_rx);
  });
  // Every connection holds a receiver; the loop waits for the last one to drop.
  let (close_tx, close_rx) = watch::channel(());
  let limits = Arc::new(limits);
  let slots = limits.max_connections.map(|n| Arc::new(Semaphore::new(n)));

  loop {
    let permit = match &slots {
      Some(slots) => tokio::select! {
        permit = Arc::clone(slots).acquire_owned() => permit.ok(),
        () = signal_tx.closed() => break,
      },
      None => None,
    };
    let (stream, peer) = tokio::select! {
      accepted = accept(&listener) => match accepted {
        Ok(accepted) => accepted,
        Err(e) => {
          if !is_connection_error(&e) {
            // Out of file descriptors and the like: back off instead of spinning.
            tokio::select! {
              () = tokio::time::sleep(Duration::from_secs(1)) => {}
              () = signal_tx.closed() => break,
            }
          }
          continue;
        }
      },
      () = signal_tx.closed() => break,
    };
    let connection = ConnectionTask {
      stream,
      service: PeerService {
        router: app.clone(),
        peer,
        served: Arc::new(AtomicUsize::new(0)),
        max_requests: limits.max_requests_per_connection,
        limit_reached: Arc::new(Notify::new()),
      },
      limits: Arc::clone(&limits),
      signal: signal_tx.clone(),
      close: close_rx.clone(),
      permit,
      transport: transport.clone(),
    };
    tokio::spawn(connection.run());
  }

  drop(close_rx);
  drop(listener);
  close_tx.closed().await;
  Ok(())
}

/// The next connection of the accept loop, with `TCP_NODELAY` set: a response (or a WebSocket
/// frame) smaller than a segment goes out at once instead of waiting up to 40 ms for the peer's
/// delayed ACK (Nagle).
async fn accept(listener: &TcpListener) -> std::io::Result<(TcpStream, SocketAddr)> {
  let (stream, peer) = listener.accept().await?;
  // A socket that refuses the option keeps Nagle on and is still served.
  let _ = stream.set_nodelay(true);
  Ok((stream, peer))
}

fn is_connection_error(e: &std::io::Error) -> bool {
  matches!(
    e.kind(),
    std::io::ErrorKind::ConnectionRefused
      | std::io::ErrorKind::ConnectionAborted
      | std::io::ErrorKind::ConnectionReset
  )
}

/// One accepted socket and what it holds until it closes.
struct ConnectionTask {
  stream: TcpStream,
  service: PeerService,
  limits: Arc<ConnectionLimits>,
  signal: watch::Sender<()>,
  close: watch::Receiver<()>,
  permit: Option<OwnedSemaphorePermit>,
  transport: HostTransport,
}

impl ConnectionTask {
  async fn run(self) {
    let Self {
      stream,
      service,
      limits,
      signal,
      close,
      permit,
      transport,
    } = self;
    // Released when the connection is done, whichever way.
    let _held = (close, permit);
    #[cfg(feature = "tls")]
    if let Some(tls) = transport.tls {
      // A shutdown during the handshake drops the socket.
      let accepted = tokio::select! {
        accepted = tls.accept(stream, limits.tls_handshake_timeout) => accepted,
        () = signal.closed() => None,
      };
      match accepted {
        Some((stream, Negotiated::Http1)) => {
          serve_connection(stream, Protocol::Http1, service, &limits, &signal).await;
        }
        Some((stream, Negotiated::Http2)) => {
          serve_connection(stream, Protocol::Http2, service, &limits, &signal).await;
        }
        // A failed handshake, or one whose ALPN (say `acme-tls/1`) carries no HTTP.
        Some((_, Negotiated::Other)) | None => {}
      }
      return;
    }
    #[cfg(not(feature = "tls"))]
    let HostTransport {} = transport;
    serve_connection(stream, Protocol::Auto, service, &limits, &signal).await;
  }
}

/// Serves one connection (plain or TLS) until it closes.
async fn serve_connection<S>(
  stream: S,
  protocol: Protocol,
  service: PeerService,
  limits: &ConnectionLimits,
  signal: &watch::Sender<()>,
) where
  S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
  let served = Arc::clone(&service.served);
  let limit_reached = Arc::clone(&service.limit_reached);
  let builder = limits.builder(protocol);
  let mut conn = pin!(
    builder.serve_connection_with_upgrades(TokioIo::new(stream), TowerToHyperService::new(service))
  );
  // hyper times out HTTP/1 heads itself, but not a client that never sends a byte (the
  // protocol sniffing waits) nor an HTTP/2 preface: the first request must arrive in time.
  let first_request = limits.header_read_timeout;
  let mut deadline = pin!(tokio::time::sleep(first_request.unwrap_or_default()));
  let mut armed = first_request.is_some();
  let mut draining = false;
  loop {
    tokio::select! {
      _done = conn.as_mut() => break,
      () = &mut deadline, if armed => {
        armed = false;
        if served.load(Ordering::Relaxed) == 0 {
          // Dropping the connection closes the socket.
          break;
        }
      }
      () = signal.closed(), if !draining => {
        draining = true;
        conn.as_mut().graceful_shutdown();
      }
      // HTTP/2 at its request limit: `GOAWAY`, the open streams finish.
      () = limit_reached.notified(), if !draining => {
        draining = true;
        conn.as_mut().graceful_shutdown();
      }
    }
  }
}

/// The router as one connection's service: records the peer and counts the requests.
#[derive(Clone)]
struct PeerService {
  router: Router,
  peer: SocketAddr,
  served: Arc<AtomicUsize>,
  max_requests: Option<usize>,
  /// Notified when an HTTP/2 connection reaches `max_requests`.
  limit_reached: Arc<Notify>,
}

impl<B> Service<Request<B>> for PeerService
where
  B: http_body::Body<Data = Bytes> + Send + 'static,
  B::Error: Into<axum::BoxError>,
{
  type Response = Response;
  type Error = Infallible;
  type Future = Pin<Box<dyn Future<Output = Result<Response, Infallible>> + Send>>;

  fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
    Poll::Ready(Ok(()))
  }

  fn call(&mut self, mut request: Request<B>) -> Self::Future {
    let n = self
      .served
      .fetch_add(1, Ordering::Relaxed)
      .saturating_add(1);
    let at_limit = self.max_requests.is_some_and(|max| n >= max);
    let close = at_limit && request.version() < Version::HTTP_2;
    if at_limit && !close {
      self.limit_reached.notify_one();
    }
    request.extensions_mut().insert(ConnectInfo(self.peer));
    let response = self.router.call(request);
    Box::pin(async move {
      let mut response = response.await?;
      if close {
        response
          .headers_mut()
          .insert(header::CONNECTION, HeaderValue::from_static("close"));
      }
      Ok(response)
    })
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  /// Certificate files in a fresh directory, as `HostTls::Files` polled every `reload`.
  #[cfg(feature = "http3")]
  fn cert_files(reload: Option<Duration>) -> crate::server::bootstrap::HostTls {
    let ck = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
    let dir = std::env::temp_dir().join(format!(
      "id-effect-conn-{}-{}",
      std::process::id(),
      reload.map_or(0, |d| d.as_millis())
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let (chain, key) = (dir.join("chain.pem"), dir.join("key.pem"));
    std::fs::write(&chain, ck.cert.pem()).unwrap();
    std::fs::write(&key, ck.signing_key.serialize_pem()).unwrap();
    crate::server::bootstrap::HostTls::Files {
      cert_file: chain,
      key_file: key,
      reload_every: reload,
    }
  }

  #[cfg(feature = "http3")]
  #[test]
  fn certificate_files_give_tls_and_http3_one_identity() {
    let config = HostConfig::default().with_tls(cert_files(None));
    let transport = HostTransport::from_config(&config).unwrap();
    let identity = transport.http3_identity().expect("an HTTP/3 identity");
    assert_eq!(transport, transport.clone());
    assert_ne!(transport, HostTransport::plain());
    // TCP presents the same slot: the fingerprints agree.
    let tcp = transport.server_tls().unwrap();
    assert!(Arc::strong_count(tcp.rustls_config()) >= 1);
    assert_eq!(identity.cert_sha256(), identity.clone().cert_sha256());
    // Polling needs a Tokio runtime; outside one the transport is refused, not built half.
    let polled = HostConfig::default().with_tls(cert_files(Some(Duration::from_millis(500))));
    let err = HostTransport::from_config(&polled).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    // Provided settings built from an identity present it on HTTP/3 too (F24); a bare
    // resolver has none.
    let provided = crate::server::tls::ServerTls::from_identity(identity).unwrap();
    let config =
      HostConfig::default().with_tls(crate::server::bootstrap::HostTls::provided(provided));
    assert_eq!(
      HostTransport::from_config(&config)
        .unwrap()
        .http3_identity()
        .map(Identity::cert_sha256),
      Some(identity.cert_sha256())
    );
    let bare = crate::server::tls::ServerTls::builder(Arc::new(identity.clone()))
      .build()
      .unwrap();
    let config = HostConfig::default().with_tls(crate::server::bootstrap::HostTls::provided(bare));
    assert!(
      HostTransport::from_config(&config)
        .unwrap()
        .http3_identity()
        .is_none()
    );
  }

  #[test]
  fn limits_default_on_and_clamp() {
    let limits = ConnectionLimits::new();
    assert_eq!(limits, ConnectionLimits::default());
    assert_eq!(limits.header_read_timeout, Some(Duration::from_secs(10)));
    assert_eq!(limits.max_connections, Some(10_000));
    assert_eq!(limits.max_requests_per_connection, Some(1_000));
    assert!(limits.keep_alive);
    let loose = limits
      .clone()
      .dangerous_no_header_read_timeout()
      .dangerous_unlimited_connections()
      .dangerous_unlimited_requests_per_connection()
      .keep_alive(false);
    assert_eq!(loose.header_read_timeout, None);
    assert_eq!(loose.max_connections, None);
    assert_eq!(loose.max_requests_per_connection, None);
    assert!(!loose.keep_alive);
    let clamped = limits.max_connections(0).max_requests_per_connection(0);
    assert_eq!(clamped.max_connections, Some(1));
    assert_eq!(clamped.max_requests_per_connection, Some(1));
    assert_eq!(
      ConnectionLimits::new()
        .max_connections(usize::MAX)
        .max_connections,
      Some(Semaphore::MAX_PERMITS)
    );
  }

  #[test]
  fn dangerous_limits_are_recorded_in_the_posture_registry() {
    use id_effect::observability::posture::dangerous_opt_outs;

    let ids = [
      DANGEROUS_NO_HEADER_READ_TIMEOUT,
      DANGEROUS_UNLIMITED_CONNECTIONS,
      DANGEROUS_UNLIMITED_REQUESTS_PER_CONNECTION,
    ];
    let _ = ConnectionLimits::new()
      .dangerous_no_header_read_timeout()
      .dangerous_unlimited_connections()
      .dangerous_unlimited_requests_per_connection();
    let recorded = dangerous_opt_outs();
    for id in ids {
      let reopens = recorded
        .iter()
        .find(|(i, _)| *i == id)
        .map(|(_, reopens)| *reopens);
      assert!(
        reopens.is_some_and(|r| r.starts_with("M1: ")),
        "{id}: {reopens:?}"
      );
    }
  }

  #[test]
  fn tls_and_http2_limits_default_on_and_their_opt_outs_are_recorded() {
    use id_effect::observability::posture::dangerous_opt_outs;

    let limits = ConnectionLimits::new();
    assert_eq!(limits.tls_handshake_timeout, Some(Duration::from_secs(10)));
    assert_eq!(limits.max_concurrent_streams, Some(100));
    let set = limits
      .clone()
      .tls_handshake_timeout(Duration::from_secs(3))
      .max_concurrent_streams(0);
    assert_eq!(set.tls_handshake_timeout, Some(Duration::from_secs(3)));
    assert_eq!(set.max_concurrent_streams, Some(1));
    let loose = limits
      .dangerous_no_tls_handshake_timeout()
      .dangerous_unlimited_concurrent_streams();
    assert_eq!(loose.tls_handshake_timeout, None);
    assert_eq!(loose.max_concurrent_streams, None);
    let recorded = dangerous_opt_outs();
    for (id, finding) in [
      (DANGEROUS_NO_TLS_HANDSHAKE_TIMEOUT, "M1: "),
      (DANGEROUS_UNLIMITED_CONCURRENT_STREAMS, "C3: "),
    ] {
      assert!(
        recorded
          .iter()
          .any(|(i, reopens)| *i == id && reopens.starts_with(finding)),
        "{id}"
      );
    }
  }

  #[tokio::test]
  async fn serve_listener_returns_once_shut_down() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let served = serve_listener(listener, Router::new(), ConnectionLimits::new(), async {});
    tokio::time::timeout(Duration::from_secs(5), served)
      .await
      .expect("the loop stops when the shutdown future completes")
      .unwrap();
  }

  #[test]
  fn host_transport_is_plain_without_tls_config() {
    let plain = HostTransport::from_config(&HostConfig::default()).unwrap();
    assert_eq!(plain, HostTransport::plain());
    assert!(!plain.is_tls());
    let quiet = HostConfig {
      http3: false,
      ..HostConfig::default()
    };
    assert_eq!(log_posture(&quiet, true), 0);
    let warned = usize::from(cfg!(feature = "http3"));
    assert_eq!(log_posture(&HostConfig::default(), true), warned);
    assert_eq!(log_posture(&HostConfig::default(), false), warned);
  }

  #[cfg(not(feature = "tls"))]
  #[test]
  fn tls_config_without_the_feature_refuses_to_serve_plain() {
    let config = HostConfig::default().with_tls(crate::server::bootstrap::HostTls::files(
      "chain.pem",
      "key.pem",
    ));
    let err = HostTransport::from_config(&config).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
  }

  #[cfg(feature = "tls")]
  #[test]
  fn tls_config_builds_a_tls_transport_or_reports_the_certificate() {
    let missing = HostConfig::default().with_tls(crate::server::bootstrap::HostTls::files(
      "/nonexistent/chain.pem",
      "/nonexistent/key.pem",
    ));
    let err = HostTransport::from_config(&missing).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    let identity = id_effect_platform::tls::Identity::from_certificate(
      &id_effect_platform::tls::NodeCertificate::self_signed("localhost").unwrap(),
    )
    .unwrap();
    let tls = ServerTls::from_identity(&identity).unwrap();
    let config =
      HostConfig::default().with_tls(crate::server::bootstrap::HostTls::provided(tls.clone()));
    let transport = HostTransport::from_config(&config).unwrap();
    assert!(transport.is_tls());
    assert_eq!(transport.server_tls(), Some(&tls));
  }

  #[tokio::test]
  async fn accepted_sockets_send_without_delay() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    // The kernel's default, which the accept loop overrides.
    let (_, plain) = tokio::join!(TcpStream::connect(addr), listener.accept());
    assert!(
      !plain.unwrap().0.nodelay().unwrap(),
      "Nagle is on by default"
    );
    let (client, accepted) = tokio::join!(TcpStream::connect(addr), accept(&listener));
    let (client, (server_side, peer)) = (client.unwrap(), accepted.unwrap());
    assert_eq!(peer, client.local_addr().unwrap());
    assert!(
      server_side.nodelay().unwrap(),
      "the accept loop turns Nagle off"
    );
    drop(client);
  }

  #[test]
  fn reset_peers_are_connection_errors() {
    use std::io::{Error, ErrorKind};
    assert!(is_connection_error(&Error::from(
      ErrorKind::ConnectionReset
    )));
    assert!(!is_connection_error(&Error::other("emfile")));
  }
}
