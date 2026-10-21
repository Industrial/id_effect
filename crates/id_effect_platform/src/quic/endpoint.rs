//! [`QuicEndpoint`]: one UDP socket, an accept loop with Retry and bounded handshakes, and the
//! [`AlpnRouter`] that hands connections to routes.

use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, PoisonError};

use tokio::sync::Semaphore;

use super::config::{QuicEndpointConfig, QuicLimits, SocketBuffers, idle_timeout, transport};
use super::congestion::{CongestionControl, RouteCongestion, Selection};
use super::router::{AlpnRoute, AlpnRouter, Counters, Route};
use super::{QuicError, socket};
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
  /// Connections whose route runs BBR ([`Route::congestion_control`]), counted when the
  /// handshake chose the ALPN.
  pub congestion_bbr: u64,
  /// Connections whose route runs CUBIC.
  pub congestion_cubic: u64,
  /// Connections closed because they sent 0-RTT data on a route that takes none (F25).
  pub refused_early_data: u64,
  /// Connections handed to a route with early data before their handshake completed (F25).
  pub early_handovers: u64,
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
  buffers: SocketBuffers,
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
  idle: quinn::IdleTimeout,
  retry: bool,
  /// The server config of the current routes; each connection gets a copy with its own
  /// congestion controller.
  server: Mutex<quinn::ServerConfig>,
}

impl Shared {
  /// The server config for one incoming connection: the current one, with a controller that
  /// runs `selection` once the route is known.
  fn server_config_for(&self, selection: &Arc<Selection>) -> quinn::ServerConfig {
    let mut config = self
      .server
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .clone();
    let congestion = Arc::new(RouteCongestion::new(Arc::clone(selection)));
    config.transport_config(Arc::new(transport(&self.limits, self.idle, congestion)));
    config
  }
}

/// The quinn server config for a rustls one and `limits` (CUBIC: the endpoint's own default;
/// connections get their route's controller from [`Shared::server_config_for`]).
fn server_config(
  tls: rustls::ServerConfig,
  limits: &QuicLimits,
  idle: quinn::IdleTimeout,
) -> Result<quinn::ServerConfig, QuicError> {
  let crypto = quinn::crypto::rustls::QuicServerConfig::try_from(tls)
    .map_err(|e| QuicError::Config(format!("tls: {e}")))?;
  let mut config = quinn::ServerConfig::with_crypto(Arc::new(crypto));
  config.transport_config(Arc::new(transport(
    limits,
    idle,
    CongestionControl::Cubic.factory(),
  )));
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
    let idle = idle_timeout(&limits)?;
    let identity = config.identity().clone();
    let router = AlpnRouter::new(limits.route_backlog);
    let server = server_config(router.tls(&identity)?, &limits, idle)?;
    let shared = Arc::new(Shared {
      router,
      counters: Counters::default(),
      limits,
      idle,
      retry: config.retry(),
      server: Mutex::new(server.clone()),
    });
    let mut endpoint_config = quinn::EndpointConfig::default();
    endpoint_config
      .max_udp_payload_size(shared.limits.max_udp_payload_size)
      .map_err(|e| QuicError::Config(format!("max_udp_payload_size: {e}")))?;
    let (socket, buffers) = socket::bind_udp(addr, config.socket_buffers())
      .map_err(|e| QuicError::Bind(e.to_string()))?;
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
        buffers,
        accept_loop,
      }),
    })
  }

  /// Serve `route`'s ALPN. New handshakes negotiate it at once (open connections are not
  /// affected); the client-certificate rule follows the routes ([`AlpnRouter::client_auth_mode`]),
  /// and its connections run the route's [congestion control](Route::congestion_control).
  pub fn register(&self, route: Route) -> Result<AlpnRoute, QuicError> {
    let shared = &self.inner.shared;
    shared.router.add(route, &self.inner.identity, |tls| {
      let config = server_config(tls, &shared.limits, shared.idle)?;
      *shared.server.lock().unwrap_or_else(PoisonError::into_inner) = config.clone();
      self.inner.endpoint.set_server_config(Some(config));
      Ok(())
    })
  }

  /// The UDP socket buffers the OS granted (see [`SocketBuffers`]; Linux reports twice the
  /// payload it keeps).
  pub fn socket_buffers(&self) -> SocketBuffers {
    self.inner.buffers
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
      congestion_bbr: get(&c.congestion_bbr),
      congestion_cubic: get(&c.congestion_cubic),
      refused_early_data: get(&c.refused_early_data),
      early_handovers: get(&c.early_handovers),
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
  let selection = Arc::new(Selection::default());
  let config = Arc::new(shared.server_config_for(&selection));
  let Ok(mut connecting) = incoming.accept_with(config) else {
    return failed();
  };
  let alpn = match tokio::time::timeout_at(deadline, connecting.handshake_data()).await {
    Ok(Ok(data)) => data
      .downcast::<quinn::crypto::rustls::HandshakeData>()
      .ok()
      .and_then(|data| data.protocol),
    _ => return failed(),
  };
  // The ALPN is chosen: from the next congestion event on, the route's controller runs.
  let congestion = alpn
    .as_deref()
    .and_then(|alpn| shared.router.congestion_control(alpn))
    .unwrap_or_default();
  selection.choose(congestion);
  Counters::bump(match congestion {
    CongestionControl::Bbr => &shared.counters.congestion_bbr,
    CongestionControl::Cubic => &shared.counters.congestion_cubic,
  });
  // The route takes it: after the handshake, or at once when it takes early data (F25).
  shared
    .router
    .complete(connecting, alpn, remote, deadline, &shared.counters)
    .await;
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::quic::{ALPN_H3, ALPN_MESH, client_config};
  use crate::tls::{NodeCertificate, client_tls_config};

  fn local() -> SocketAddr {
    "127.0.0.1:0".parse().expect("addr")
  }

  /// D9: on one shared endpoint, `h3` connections run BBR and mesh connections CUBIC (the
  /// switch happens once the handshake has chosen the ALPN; data then flows both ways).
  #[tokio::test]
  async fn h3_route_uses_bbr_mesh_uses_cubic() {
    let cert = NodeCertificate::self_signed("localhost").expect("cert");
    let identity = Identity::from_certificate(&cert).expect("identity");
    let endpoint =
      QuicEndpoint::bind(local(), QuicEndpointConfig::new(identity.clone())).expect("bind");
    assert_eq!(Route::h3().congestion_control(), CongestionControl::Bbr);
    let h3 = endpoint.register(Route::h3()).expect("h3");
    let mesh = endpoint
      .register(Route::mesh(None, vec![cert.cert_sha256()]))
      .expect("mesh");
    let router = endpoint.router();
    assert_eq!(
      router.congestion_control(ALPN_H3),
      Some(CongestionControl::Bbr)
    );
    assert_eq!(
      router.congestion_control(ALPN_MESH),
      Some(CongestionControl::Cubic)
    );
    assert_eq!(router.congestion_control(b"other"), None);

    let client = quinn::Endpoint::client(local()).expect("client");
    for (alpn, route) in [(ALPN_H3, &h3), (ALPN_MESH, &mesh)] {
      let tls = client_tls_config(&identity, None, &[cert.cert_sha256()]).expect("tls");
      let config = client_config(tls, &[alpn], &QuicLimits::default()).expect("config");
      let conn = client
        .connect_with(config, endpoint.local_addr(), "localhost")
        .expect("connect")
        .await
        .expect("handshake");
      let served = route.accept().await.expect("running").expect("delivered");
      let (mut send, _recv) = served.open_bi().await.expect("open");
      send.write_all(&[7u8; 256 * 1024]).await.expect("write");
      send.finish().expect("finish");
      let (_send, mut recv) = conn.accept_bi().await.expect("accept");
      let body = recv.read_to_end(1 << 20).await.expect("read");
      assert_eq!(body.len(), 256 * 1024);
    }
    let stats = endpoint.stats();
    assert_eq!((stats.congestion_bbr, stats.congestion_cubic), (1, 1));
    assert_eq!(stats.accepted, 2);
  }

  /// F23: the endpoint asks for 4 MiB socket buffers by default and reports what it got; a
  /// size the OS can grant is applied in full, a shortfall is logged and reported as is.
  #[tokio::test]
  async fn socket_buffers_are_applied_or_the_shortfall_logged() {
    let identity =
      Identity::from_certificate(&NodeCertificate::self_signed("x").expect("cert")).expect("id");
    let config = QuicEndpointConfig::new(identity.clone());
    assert_eq!(config.socket_buffers(), SocketBuffers::default());
    assert_eq!(
      SocketBuffers::default().recv,
      super::super::DEFAULT_SOCKET_BUFFER
    );
    let endpoint = QuicEndpoint::bind(local(), config).expect("bind");
    let granted = endpoint.socket_buffers();
    assert!(granted.recv > 0 && granted.send > 0, "{granted:?}");

    let small = SocketBuffers::new(128 * 1024, 128 * 1024);
    let endpoint = QuicEndpoint::bind(
      local(),
      QuicEndpointConfig::new(identity).with_socket_buffers(small),
    )
    .expect("bind");
    let granted = endpoint.socket_buffers();
    assert!(
      granted.recv >= small.recv && granted.send >= small.send,
      "{granted:?}"
    );
  }
}
