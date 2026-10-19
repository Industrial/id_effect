//! [`QuicEndpoint`]: one UDP socket, an accept loop with Retry and bounded handshakes, and the
//! [`AlpnRouter`] that hands connections to routes.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use tokio::sync::Semaphore;

use super::QuicError;
use super::config::{QuicEndpointConfig, QuicLimits, transport_config};
use super::router::{AlpnRoute, AlpnRouter, Counters, Refusal, Route};
use crate::tls::Identity;

/// A snapshot of an endpoint's counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct EndpointStats {
  /// Retry packets sent (address validation).
  pub retries_sent: u64,
  /// Connections handed to a route.
  pub accepted: u64,
  /// Connections closed because their route requires a verified client chain (D7).
  pub refused_client_cert: u64,
  /// Connections closed because no route takes their ALPN any more.
  pub refused_no_route: u64,
  /// Attempts refused because too many handshakes were pending or a route's backlog was full.
  pub refused_busy: u64,
  /// Handshakes that failed or timed out (bad certificates, unknown ALPN, silent peers).
  pub handshakes_failed: u64,
}

/// One QUIC endpoint on one UDP port, routing connections by ALPN (see the
/// [module docs](super)). Cloning shares the endpoint; it stops accepting when the last clone is
/// dropped (open connections continue).
#[derive(Clone, Debug)]
pub struct QuicEndpoint {
  inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
  endpoint: quinn::Endpoint,
  shared: Arc<Shared>,
  identity: Identity,
  local_addr: SocketAddr,
  accept_loop: tokio::task::JoinHandle<()>,
}

impl Drop for Inner {
  fn drop(&mut self) {
    self.accept_loop.abort();
  }
}

/// What the accept loop and the handshake tasks share.
#[derive(Debug)]
struct Shared {
  router: AlpnRouter,
  counters: Counters,
  limits: QuicLimits,
  retry: bool,
}

/// The quinn server config for a rustls one and `limits`.
fn server_config(
  tls: rustls::ServerConfig,
  limits: &QuicLimits,
) -> Result<quinn::ServerConfig, QuicError> {
  let crypto = quinn::crypto::rustls::QuicServerConfig::try_from(tls)
    .map_err(|e| QuicError::Config(format!("tls: {e}")))?;
  let mut config = quinn::ServerConfig::with_crypto(Arc::new(crypto));
  config.transport_config(Arc::new(transport_config(limits)?));
  Ok(config)
}

impl QuicEndpoint {
  /// Bind `addr` (UDP) with `config`, on the current Tokio runtime. The endpoint negotiates no
  /// ALPN until a route is [registered](Self::register).
  pub fn bind(addr: SocketAddr, config: QuicEndpointConfig) -> Result<Self, QuicError> {
    config.validate()?;
    let runtime =
      quinn::default_runtime().ok_or_else(|| QuicError::Bind("no async runtime".into()))?;
    let limits = config.limits().clone();
    let shared = Arc::new(Shared {
      router: AlpnRouter::new(limits.route_backlog),
      counters: Counters::default(),
      limits,
      retry: config.retry(),
    });
    let identity = config.identity().clone();
    let server = server_config(shared.router.tls(&identity)?, &shared.limits)?;
    let mut endpoint_config = quinn::EndpointConfig::default();
    endpoint_config
      .max_udp_payload_size(shared.limits.max_udp_payload_size)
      .map_err(|e| QuicError::Config(format!("max_udp_payload_size: {e}")))?;
    let socket = std::net::UdpSocket::bind(addr).map_err(|e| QuicError::Bind(e.to_string()))?;
    let endpoint = quinn::Endpoint::new(endpoint_config, Some(server), socket, runtime)
      .map_err(|e| QuicError::Bind(e.to_string()))?;
    let local_addr = endpoint
      .local_addr()
      .map_err(|e| QuicError::Bind(e.to_string()))?;
    let accept_loop = tokio::spawn(accept_loop(endpoint.clone(), Arc::clone(&shared)));
    Ok(Self {
      inner: Arc::new(Inner {
        endpoint,
        shared,
        identity,
        local_addr,
        accept_loop,
      }),
    })
  }

  /// Serve `route`'s ALPN. New handshakes negotiate it at once (open connections are not
  /// affected); the client-certificate rule follows the routes ([`AlpnRouter::client_auth_mode`]).
  pub fn register(&self, route: Route) -> Result<AlpnRoute, QuicError> {
    let shared = &self.inner.shared;
    shared.router.add(route, &self.inner.identity, |tls| {
      let config = server_config(tls, &shared.limits)?;
      self.inner.endpoint.set_server_config(Some(config));
      Ok(())
    })
  }

  /// Dial `addr` from this endpoint's socket with `config` (see
  /// [`client_config`](super::client_config)), verifying the server for `server_name`.
  pub async fn connect(
    &self,
    addr: SocketAddr,
    server_name: &str,
    config: quinn::ClientConfig,
  ) -> Result<quinn::Connection, QuicError> {
    self
      .inner
      .endpoint
      .connect_with(config, addr, server_name)
      .map_err(|e| QuicError::Connect(e.to_string()))?
      .await
      .map_err(|e| QuicError::Connect(e.to_string()))
  }

  /// The bound address (resolves an ephemeral port).
  pub fn local_addr(&self) -> SocketAddr {
    self.inner.local_addr
  }

  /// The routing table.
  pub fn router(&self) -> &AlpnRouter {
    &self.inner.shared.router
  }

  /// The limits in force.
  pub fn limits(&self) -> &QuicLimits {
    &self.inner.shared.limits
  }

  /// Whether address validation (Retry) is on.
  pub fn retry_enabled(&self) -> bool {
    self.inner.shared.retry
  }

  /// A snapshot of the counters.
  pub fn stats(&self) -> EndpointStats {
    let c = &self.inner.shared.counters;
    let get = |counter: &std::sync::atomic::AtomicU64| counter.load(Ordering::Relaxed);
    EndpointStats {
      retries_sent: get(&c.retries_sent),
      accepted: get(&c.accepted),
      refused_client_cert: get(&c.refused_client_cert),
      refused_no_route: get(&c.refused_no_route),
      refused_busy: get(&c.refused_busy),
      handshakes_failed: get(&c.handshakes_failed),
    }
  }

  /// Close every connection and stop accepting; every [`AlpnRoute`] then yields `None`.
  pub fn close(&self) {
    // The accept loop ends on its own once the closed endpoint yields no more attempts.
    self.inner.shared.router.close();
    self.inner.endpoint.close(0u32.into(), b"endpoint closed");
  }
}

/// Validate addresses, bound the handshakes in flight, and run each in its own task.
async fn accept_loop(endpoint: quinn::Endpoint, shared: Arc<Shared>) {
  let pending = Arc::new(Semaphore::new(shared.limits.max_pending_handshakes));
  while let Some(incoming) = endpoint.accept().await {
    if shared.retry && !incoming.remote_address_validated() && incoming.may_retry() {
      Counters::bump(&shared.counters.retries_sent);
      let _ = incoming.retry();
      continue;
    }
    let Ok(permit) = Arc::clone(&pending).try_acquire_owned() else {
      Counters::bump(&shared.counters.refused_busy);
      incoming.refuse();
      continue;
    };
    let shared = Arc::clone(&shared);
    tokio::spawn(async move {
      handshake(incoming, &shared).await;
      drop(permit);
    });
  }
}

/// Complete one handshake within the limit and dispatch the connection; a failure after the
/// ALPN was chosen is reported to that ALPN's route.
async fn handshake(incoming: quinn::Incoming, shared: &Shared) {
  let remote = incoming.remote_address();
  let deadline = tokio::time::Instant::now() + shared.limits.handshake_timeout;
  let failed = || Counters::bump(&shared.counters.handshakes_failed);
  let Ok(mut connecting) = incoming.accept() else {
    return failed();
  };
  let alpn = match tokio::time::timeout_at(deadline, connecting.handshake_data()).await {
    Ok(Ok(data)) => data
      .downcast::<quinn::crypto::rustls::HandshakeData>()
      .ok()
      .and_then(|data| data.protocol),
    _ => return failed(),
  };
  let reason = match tokio::time::timeout_at(deadline, connecting).await {
    Ok(Ok(conn)) => return shared.router.dispatch(conn, &shared.counters),
    Ok(Err(e)) => e.to_string(),
    Err(_) => "handshake timed out".to_string(),
  };
  failed();
  if let Some(alpn) = alpn {
    shared
      .router
      .refuse(&alpn, Refusal::Handshake { remote, reason });
  }
}
