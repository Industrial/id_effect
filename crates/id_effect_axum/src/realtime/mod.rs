//! Realtime endpoints: WebSocket ([`ws`], feature `ws`) and server-sent events ([`sse`], feature
//! `sse`), secure by default (ADR 0013, ADR 0014 §14 C3/C4).
//!
//! Both are typed routes: [`ws::endpoint`] and [`sse::endpoint`] return an
//! [`Endpoint`](crate::route::Endpoint) without an access decision, so
//! [`Router::route`](crate::route::Router::route) only accepts it after `.public()` or
//! `.guard(g)` (H8), and the router's deny-by-default [`AccessLayer`](crate::route::AccessLayer)
//! covers it. Authentication runs on connect: an `Authenticate` layer (`id_effect_auth`) and the
//! route's guards see the upgrade request like any other, and the handler receives the
//! request's [`RequestCaps`](crate::RequestCaps) (the principal, the tenant).
//!
//! | Default | WebSocket | SSE |
//! |---|---|---|
//! | Origin allowlist (R9) | empty: every browser origin refused ([`origin`]) | — (no ambient-credential upgrade; use CORS) |
//! | Frame / message size (R4) | 64 KiB each; above it the socket closes with 1009 | events are server-written |
//! | Message rate (R4) | 100 inbound messages per second per socket (burst 100); above it 1008 | — |
//! | Concurrent connections (R4) | 16 per client IP, 10 000 per endpoint: **429** before the upgrade | same |
//! | Heartbeat | ping every 20 s; two missed pongs close the socket (1001) | a comment every 15 s |
//! | Idle timeout | 300 s without a message either way (1001) | — |
//! | Backpressure | bounded inbox and outbox (16 messages each): a slow handler stops the reads, a slow client suspends `send` | the producer waits for the client |
//!
//! Time is the executor clock (`sleep` / `now` of the fiber's executor), so heartbeats, idle
//! timeouts and rate limits run in virtual time on a [`TestExecutor`](id_effect::TestExecutor).
//!
//! The client address for the per-IP cap is [`ClientIp`](crate::ClientIp): the peer, or the
//! `X-Forwarded-For` hop behind [`TrustedProxies`](crate::TrustedProxies) set as a request
//! extension.

#[cfg(any(feature = "ws", feature = "webtransport"))]
pub mod origin;
#[cfg(feature = "sse")]
pub mod sse;
#[cfg(feature = "ws")]
pub mod ws;

#[cfg(any(feature = "ws", feature = "sse"))]
pub use limits::{ConnectionCap, DEFAULT_MAX_CONNECTIONS, DEFAULT_MAX_CONNECTIONS_PER_IP};

#[cfg(any(feature = "ws", feature = "sse"))]
mod limits {
  use std::collections::HashMap;
  use std::net::IpAddr;
  use std::sync::{Arc, Mutex, PoisonError};

  use axum::http::{HeaderValue, StatusCode, header};
  use axum::response::{IntoResponse, Response};

  use crate::route::Problem;

  /// Default open connections per realtime endpoint: 10 000.
  pub const DEFAULT_MAX_CONNECTIONS: usize = 10_000;
  /// Default open connections per client IP and endpoint: 16.
  pub const DEFAULT_MAX_CONNECTIONS_PER_IP: usize = 16;

  #[derive(Debug, Default)]
  struct Slots {
    total: usize,
    per_ip: HashMap<IpAddr, usize>,
  }

  /// Bounds the open connections of one realtime endpoint: in total and per client IP. Cheap
  /// to clone (clones share the counts); pass one cap to several endpoints to share it.
  #[derive(Clone, Debug)]
  pub struct ConnectionCap {
    inner: Arc<Inner>,
  }

  #[derive(Debug)]
  struct Inner {
    max_total: usize,
    max_per_ip: usize,
    slots: Mutex<Slots>,
  }

  impl ConnectionCap {
    /// At most `max_total` connections, `max_per_ip` from one client IP (each at least 1).
    pub fn new(max_total: usize, max_per_ip: usize) -> Self {
      Self {
        inner: Arc::new(Inner {
          max_total: max_total.max(1),
          max_per_ip: max_per_ip.max(1),
          slots: Mutex::new(Slots::default()),
        }),
      }
    }

    /// The total limit.
    pub fn max_total(&self) -> usize {
      self.inner.max_total
    }

    /// The per-IP limit.
    pub fn max_per_ip(&self) -> usize {
      self.inner.max_per_ip
    }

    /// The connections open now.
    pub fn open(&self) -> usize {
      self.slots().total
    }

    /// The connections open now from `ip`.
    pub fn open_from(&self, ip: IpAddr) -> usize {
      self.slots().per_ip.get(&ip).copied().unwrap_or(0)
    }

    fn slots(&self) -> std::sync::MutexGuard<'_, Slots> {
      self
        .inner
        .slots
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
    }

    /// Takes a slot for a connection from `ip` (`None`: the address is unknown, only the total
    /// counts), released when the permit drops.
    pub(crate) fn acquire(&self, ip: Option<IpAddr>) -> Result<Permit, CapReached> {
      let mut slots = self.slots();
      if slots.total >= self.inner.max_total {
        return Err(CapReached::Total);
      }
      if let Some(ip) = ip {
        let n = slots.per_ip.entry(ip).or_insert(0);
        if *n >= self.inner.max_per_ip {
          return Err(CapReached::PerIp);
        }
        *n += 1;
      }
      slots.total += 1;
      Ok(Permit {
        cap: self.clone(),
        ip,
      })
    }
  }

  /// One open connection's slot.
  #[derive(Debug)]
  pub(crate) struct Permit {
    cap: ConnectionCap,
    ip: Option<IpAddr>,
  }

  impl Drop for Permit {
    fn drop(&mut self) {
      let mut slots = self.cap.slots();
      slots.total = slots.total.saturating_sub(1);
      if let Some(ip) = self.ip
        && let Some(n) = slots.per_ip.get_mut(&ip)
      {
        *n -= 1;
        if *n == 0 {
          slots.per_ip.remove(&ip);
        }
      }
    }
  }

  /// The cap refused a connection: **429** Problem Details with `Retry-After: 1`.
  #[derive(Clone, Copy, Debug, PartialEq, Eq)]
  pub(crate) enum CapReached {
    Total,
    PerIp,
  }

  impl IntoResponse for CapReached {
    fn into_response(self) -> Response {
      let detail = match self {
        CapReached::Total => "the endpoint has reached its connection limit",
        CapReached::PerIp => "too many open connections from this client address",
      };
      let mut res = Problem::new(StatusCode::TOO_MANY_REQUESTS)
        .with_title("Too many connections")
        .with_detail(detail)
        .into_response();
      res
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
      res
    }
  }

  #[cfg(test)]
  mod tests {
    use super::*;

    #[test]
    fn caps_total_and_per_ip_and_releases_on_drop() {
      let cap = ConnectionCap::new(3, 2);
      let a: IpAddr = "192.0.2.1".parse().unwrap();
      let b: IpAddr = "192.0.2.2".parse().unwrap();
      let p1 = cap.acquire(Some(a)).unwrap();
      let p2 = cap.acquire(Some(a)).unwrap();
      assert_eq!(cap.acquire(Some(a)).unwrap_err(), CapReached::PerIp);
      let p3 = cap.acquire(Some(b)).unwrap();
      assert_eq!(cap.acquire(None).unwrap_err(), CapReached::Total);
      assert_eq!((cap.open(), cap.open_from(a), cap.open_from(b)), (3, 2, 1));
      drop((p1, p3));
      assert_eq!((cap.open(), cap.open_from(a), cap.open_from(b)), (1, 1, 0));
      let p4 = cap.acquire(None).unwrap();
      drop((p2, p4));
      assert_eq!(cap.open(), 0);
      assert_eq!((cap.max_total(), cap.max_per_ip()), (3, 2));
      let zero = ConnectionCap::new(0, 0);
      assert_eq!((zero.max_total(), zero.max_per_ip()), (1, 1));
      for reached in [CapReached::Total, CapReached::PerIp] {
        let res = reached.into_response();
        assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(res.headers()[header::RETRY_AFTER], "1");
      }
    }
  }
}
