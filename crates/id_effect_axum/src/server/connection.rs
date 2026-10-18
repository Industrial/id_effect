//! The accept loop behind every server entry point ([`serve_listener`]): one hyper-util
//! connection per accepted socket, bounded by [`ConnectionLimits`] (header-read timeout,
//! keep-alive limit, connection cap), with the peer address recorded on every request as
//! `ConnectInfo<SocketAddr>` (so [`ClientIp`](crate::ClientIp) and per-IP rate limits see it).
//!
//! Graceful shutdown is axum's: when the shutdown future completes the listener stops accepting,
//! every open connection finishes its in-flight requests and closes, and the loop returns once
//! the last one has.
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
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};
use tower_service::Service;

/// Per-connection limits of the server (slowloris and connection exhaustion). The defaults are
/// on; each `dangerous_*` method turns one off.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectionLimits {
  header_read_timeout: Option<Duration>,
  max_connections: Option<usize>,
  keep_alive: bool,
  max_requests_per_connection: Option<usize>,
}

impl Default for ConnectionLimits {
  /// [`Self::DEFAULT_HEADER_READ_TIMEOUT`], [`Self::DEFAULT_MAX_CONNECTIONS`], keep-alive on with
  /// [`Self::DEFAULT_MAX_REQUESTS_PER_CONNECTION`].
  fn default() -> Self {
    Self {
      header_read_timeout: Some(Self::DEFAULT_HEADER_READ_TIMEOUT),
      max_connections: Some(Self::DEFAULT_MAX_CONNECTIONS),
      keep_alive: true,
      max_requests_per_connection: Some(Self::DEFAULT_MAX_REQUESTS_PER_CONNECTION),
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

  /// The defaults (see [`Default`]).
  pub fn new() -> Self {
    Self::default()
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
  /// behind a proxy that enforces its own timeout.
  #[must_use]
  pub fn dangerous_no_header_read_timeout(mut self) -> Self {
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
  #[must_use]
  pub fn dangerous_unlimited_connections(mut self) -> Self {
    self.max_connections = None;
    self
  }

  /// Whether HTTP/1 connections are kept alive between requests (default `true`).
  #[must_use]
  pub fn keep_alive(mut self, enabled: bool) -> Self {
    self.keep_alive = enabled;
    self
  }

  /// Close an HTTP/1 keep-alive connection after `n` (at least 1) requests: the `n`th response
  /// carries `Connection: close`.
  #[must_use]
  pub fn max_requests_per_connection(mut self, n: usize) -> Self {
    self.max_requests_per_connection = Some(n.max(1));
    self
  }

  /// No limit on the requests one HTTP/1 keep-alive connection may make.
  ///
  /// **Loosens the keep-alive limit** (finding M1 of the 0.10.1 security review): one client
  /// may hold a connection, and its slot under [`Self::max_connections`], for as long as it
  /// keeps sending requests.
  #[must_use]
  pub fn dangerous_unlimited_requests_per_connection(mut self) -> Self {
    self.max_requests_per_connection = None;
    self
  }

  fn builder(&self) -> Builder<TokioExecutor> {
    let mut builder = Builder::new(TokioExecutor::new());
    builder
      .http1()
      .timer(TokioTimer::new())
      .header_read_timeout(self.header_read_timeout)
      .keep_alive(self.keep_alive);
    builder
  }
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
      accepted = listener.accept() => match accepted {
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
      },
      limits: Arc::clone(&limits),
      signal: signal_tx.clone(),
      close: close_rx.clone(),
      permit,
    };
    tokio::spawn(connection.run());
  }

  drop(close_rx);
  drop(listener);
  close_tx.closed().await;
  Ok(())
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
    } = self;
    let served = Arc::clone(&service.served);
    let builder = limits.builder();
    let mut conn = pin!(
      builder
        .serve_connection_with_upgrades(TokioIo::new(stream), TowerToHyperService::new(service))
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
      }
    }
    drop(close);
    drop(permit);
  }
}

/// The router as one connection's service: records the peer and counts the requests.
#[derive(Clone)]
struct PeerService {
  router: Router,
  peer: SocketAddr,
  served: Arc<AtomicUsize>,
  max_requests: Option<usize>,
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
    let close =
      request.version() < Version::HTTP_2 && self.max_requests.is_some_and(|max| n >= max);
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
  fn reset_peers_are_connection_errors() {
    use std::io::{Error, ErrorKind};
    assert!(is_connection_error(&Error::from(
      ErrorKind::ConnectionReset
    )));
    assert!(!is_connection_error(&Error::other("emfile")));
  }
}
