//! The client's IP address, behind reverse proxies you trust: [`ClientIp`].
//!
//! The address starts as the connection's peer (`ConnectInfo<SocketAddr>`, which
//! [`serve`](crate::serve) and the other server entry points record on every request). Only when
//! that peer is one of the [`TrustedProxies`] is `X-Forwarded-For` read, from the **right**: each
//! trusted proxy appends the address it saw, so the rightmost entry that is not a trusted proxy
//! is the client. Entries left of it were written by the client and are ignored, so prepending
//! made-up addresses changes nothing. With no trusted proxies (the default) the header is never
//! read.
//!
//! ```rust
//! use axum::{Extension, Router, routing::get};
//! use id_effect_axum::{ClientIp, TrustedProxies};
//!
//! // The load balancer in front of the service sits in 10.0.0.0/8.
//! let trusted = TrustedProxies::new(["10.0.0.0/8"]).expect("valid CIDRs");
//! let app: Router = Router::new()
//!   .route("/ip", get(|ClientIp(ip): ClientIp| async move { ip.to_string() }))
//!   .layer(Extension(trusted));
//! # let _ = app;
//! ```

use std::fmt;
use std::net::{IpAddr, SocketAddr};

use axum::extract::connect_info::MockConnectInfo;
use axum::extract::{ConnectInfo, FromRequestParts};
use axum::http::request::Parts;
use axum::http::{Extensions, HeaderMap, HeaderName, StatusCode};
use axum::response::{IntoResponse, Response};

/// `X-Forwarded-For`.
pub const X_FORWARDED_FOR: HeaderName = HeaderName::from_static("x-forwarded-for");

/// One trusted network: an address and its prefix length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Cidr {
  addr: IpAddr,
  prefix: u8,
}

impl Cidr {
  fn parse(text: &str) -> Option<Self> {
    let (addr, prefix) = match text.split_once('/') {
      Some((addr, prefix)) => (addr, Some(prefix)),
      None => (text, None),
    };
    let addr = addr.trim().parse::<IpAddr>().ok()?.to_canonical();
    let max = if addr.is_ipv4() { 32 } else { 128 };
    let prefix = match prefix {
      Some(prefix) => prefix.trim().parse::<u8>().ok().filter(|p| *p <= max)?,
      None => max,
    };
    Some(Self { addr, prefix })
  }

  fn contains(&self, ip: IpAddr) -> bool {
    match (self.addr, ip) {
      (IpAddr::V4(net), IpAddr::V4(ip)) => masked(
        u128::from(net.to_bits()),
        u128::from(ip.to_bits()),
        self.prefix,
        32,
      ),
      (IpAddr::V6(net), IpAddr::V6(ip)) => masked(net.to_bits(), ip.to_bits(), self.prefix, 128),
      _ => false,
    }
  }
}

/// Whether the top `prefix` of `width` bits of `a` and `b` agree.
fn masked(a: u128, b: u128, prefix: u8, width: u32) -> bool {
  let shift = width - u32::from(prefix);
  shift >= width || (a >> shift) == (b >> shift)
}

impl fmt::Display for Cidr {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{}/{}", self.addr, self.prefix)
  }
}

/// The reverse proxies whose `X-Forwarded-For` entries [`ClientIp`] believes, as CIDRs
/// (`10.0.0.0/8`, `::1/128`; a bare address is a single host). Empty by default: the socket
/// peer is the client.
///
/// [`ClientIp`]'s extractor reads them from a request extension
/// (`.layer(Extension(trusted))`); without one it trusts no proxy.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrustedProxies {
  nets: Vec<Cidr>,
}

impl TrustedProxies {
  /// No trusted proxy: [`ClientIp`] is the socket peer and `X-Forwarded-For` is ignored.
  pub fn none() -> Self {
    Self::default()
  }

  /// The proxies in `cidrs`.
  ///
  /// # Errors
  ///
  /// [`InvalidTrustedProxy`] naming the first entry that is not an address or a CIDR.
  pub fn new<I, S>(cidrs: I) -> Result<Self, InvalidTrustedProxy>
  where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
  {
    let nets = cidrs
      .into_iter()
      .map(|cidr| {
        let cidr = cidr.as_ref();
        Cidr::parse(cidr).ok_or_else(|| InvalidTrustedProxy {
          value: cidr.to_owned(),
        })
      })
      .collect::<Result<_, _>>()?;
    Ok(Self { nets })
  }

  /// Whether `ip` is in one of the trusted networks (IPv4-mapped IPv6 addresses count as IPv4).
  pub fn is_trusted(&self, ip: IpAddr) -> bool {
    let ip = ip.to_canonical();
    self.nets.iter().any(|net| net.contains(ip))
  }

  /// Whether no proxy is trusted.
  pub fn is_empty(&self) -> bool {
    self.nets.is_empty()
  }
}

/// A trusted proxy entry that is neither an IP address nor a CIDR.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvalidTrustedProxy {
  /// The entry as given.
  pub value: String,
}

impl fmt::Display for InvalidTrustedProxy {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(
      f,
      "invalid trusted proxy `{}`: expected an IP address or a CIDR",
      self.value
    )
  }
}

impl std::error::Error for InvalidTrustedProxy {}

/// The client's IP address (see the [module docs](self)); an extractor.
///
/// As an extractor it reads the [`TrustedProxies`] request extension (none: trust no proxy) and
/// rejects with [`ClientIpRejection`] (`500`) when the app is not served with connect info.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ClientIp(pub IpAddr);

impl ClientIp {
  /// The client of a connection from `peer` carrying `headers`: `peer` unless it is a trusted
  /// proxy, else the rightmost `X-Forwarded-For` entry that is not a trusted proxy (or the
  /// leftmost entry when every one is). An entry that is not an address stops the walk at the
  /// proxy that appended it.
  pub fn resolve(peer: IpAddr, headers: &HeaderMap, trusted: &TrustedProxies) -> Self {
    let mut client = peer.to_canonical();
    if !trusted.is_trusted(client) {
      return Self(client);
    }
    let hops: Vec<Option<IpAddr>> = headers
      .get_all(X_FORWARDED_FOR)
      .iter()
      .flat_map(|value| match value.to_str() {
        Ok(text) => text.split(',').map(parse_hop).collect::<Vec<_>>(),
        Err(_) => vec![None],
      })
      .collect();
    for hop in hops.into_iter().rev() {
      let Some(hop) = hop else { break };
      client = hop.to_canonical();
      if !trusted.is_trusted(client) {
        break;
      }
    }
    Self(client)
  }

  /// The client of a request with `extensions` and `headers`: [`Self::resolve`] on the peer of
  /// `ConnectInfo<SocketAddr>` (or axum's `MockConnectInfo<SocketAddr>` in tests); `None`
  /// without one.
  pub fn from_parts(
    extensions: &Extensions,
    headers: &HeaderMap,
    trusted: &TrustedProxies,
  ) -> Option<Self> {
    let peer = extensions
      .get::<ConnectInfo<SocketAddr>>()
      .map(|ConnectInfo(addr)| *addr)
      .or_else(|| {
        extensions
          .get::<MockConnectInfo<SocketAddr>>()
          .map(|MockConnectInfo(addr)| *addr)
      })?;
    Some(Self::resolve(peer.ip(), headers, trusted))
  }
}

/// One `X-Forwarded-For` entry: an address, optionally with a port (`[::1]:80`, `10.0.0.1:80`).
fn parse_hop(entry: &str) -> Option<IpAddr> {
  let entry = entry.trim();
  entry
    .parse::<IpAddr>()
    .ok()
    .or_else(|| entry.parse::<SocketAddr>().ok().map(|addr| addr.ip()))
}

impl fmt::Display for ClientIp {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    self.0.fmt(f)
  }
}

/// [`ClientIp`]'s rejection: the request carries no connection info (the router is not served
/// by [`serve`](crate::serve) or with `into_make_service_with_connect_info::<SocketAddr>()`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClientIpRejection;

impl fmt::Display for ClientIpRejection {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("the client address is unknown: the app is not served with connect info")
  }
}

impl std::error::Error for ClientIpRejection {}

impl IntoResponse for ClientIpRejection {
  fn into_response(self) -> Response {
    (StatusCode::INTERNAL_SERVER_ERROR, self.to_string()).into_response()
  }
}

impl<S: Send + Sync> FromRequestParts<S> for ClientIp {
  type Rejection = ClientIpRejection;

  async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
    let none = TrustedProxies::none();
    let trusted = parts.extensions.get::<TrustedProxies>().unwrap_or(&none);
    Self::from_parts(&parts.extensions, &parts.headers, trusted).ok_or(ClientIpRejection)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use axum::http::HeaderValue;

  fn ip(text: &str) -> IpAddr {
    text.parse().unwrap()
  }

  fn forwarded(values: &[&str]) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for value in values {
      headers.append(X_FORWARDED_FOR, HeaderValue::from_str(value).unwrap());
    }
    headers
  }

  #[test]
  fn cidrs_parse_and_match() {
    let trusted = TrustedProxies::new(["10.0.0.0/8", "192.0.2.7", "fd00::/8", "0.0.0.0/0"]);
    let trusted = trusted.unwrap();
    assert!(trusted.is_trusted(ip("10.200.0.1")));
    assert!(trusted.is_trusted(ip("::ffff:10.1.2.3")));
    assert!(trusted.is_trusted(ip("fd12::1")));
    assert!(!trusted.is_trusted(ip("fe80::1")));
    let narrow = TrustedProxies::new(["192.0.2.7", "2001:db8::/32"]).unwrap();
    assert!(narrow.is_trusted(ip("192.0.2.7")));
    assert!(!narrow.is_trusted(ip("192.0.2.8")));
    assert!(narrow.is_trusted(ip("2001:db8:1::1")));
    assert!(!narrow.is_trusted(ip("2001:db9::1")));
    assert!(TrustedProxies::none().is_empty() && !narrow.is_empty());
    for bad in ["10.0.0.0/33", "::/129", "nope", "10.0.0.0/x"] {
      let err = TrustedProxies::new([bad]).unwrap_err();
      assert_eq!(err.value, bad);
      assert!(err.to_string().contains(bad));
    }
    assert_eq!(Cidr::parse("10.0.0.0/8").unwrap().to_string(), "10.0.0.0/8");
  }

  #[test]
  fn h1_forwarded_for_parsed_from_the_right() {
    let proxy = ip("10.0.0.2");
    let trusted = TrustedProxies::new(["10.0.0.0/8"]).unwrap();
    // The client prepends a spoofed hop; the proxy appends the real one.
    let headers = forwarded(&["6.6.6.6, 198.51.100.1"]);
    assert_eq!(
      ClientIp::resolve(proxy, &headers, &trusted).0,
      ip("198.51.100.1")
    );
    // Two trusted hops are skipped, across several header lines, ports allowed.
    let headers = forwarded(&["6.6.6.6", "198.51.100.1:5000, 10.0.0.9", "10.0.0.3"]);
    assert_eq!(
      ClientIp::resolve(proxy, &headers, &trusted).0,
      ip("198.51.100.1")
    );
    // Every hop trusted: the leftmost one.
    let headers = forwarded(&["10.0.0.7, 10.0.0.8"]);
    assert_eq!(
      ClientIp::resolve(proxy, &headers, &trusted).0,
      ip("10.0.0.7")
    );
    // Garbage stops the walk at the proxy that appended it.
    let headers = forwarded(&["6.6.6.6, garbage, 10.0.0.9"]);
    assert_eq!(
      ClientIp::resolve(proxy, &headers, &trusted).0,
      ip("10.0.0.9")
    );
    // An untrusted peer's header is ignored; so is every header without trusted proxies.
    let headers = forwarded(&["6.6.6.6"]);
    assert_eq!(
      ClientIp::resolve(ip("203.0.113.5"), &headers, &trusted).0,
      ip("203.0.113.5")
    );
    assert_eq!(
      ClientIp::resolve(proxy, &headers, &TrustedProxies::none()).0,
      proxy
    );
    // No header behind a trusted proxy: the proxy.
    assert_eq!(
      ClientIp::resolve(proxy, &HeaderMap::new(), &trusted).0,
      proxy
    );
    // IPv6 and IPv4-mapped peers.
    let headers = forwarded(&["[2001:db8::1]:443"]);
    assert_eq!(
      ClientIp::resolve(ip("::ffff:10.0.0.2"), &headers, &trusted).0,
      ip("2001:db8::1")
    );
    assert_eq!(ClientIp(proxy).to_string(), "10.0.0.2");
  }

  #[tokio::test]
  async fn the_extractor_needs_connect_info() {
    let mut parts = axum::http::Request::new(()).into_parts().0;
    assert_eq!(
      ClientIp::from_request_parts(&mut parts, &()).await,
      Err(ClientIpRejection)
    );
    let response = ClientIpRejection.into_response();
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    parts
      .extensions
      .insert(MockConnectInfo(SocketAddr::from(([10, 0, 0, 2], 1))));
    parts
      .headers
      .insert(X_FORWARDED_FOR, HeaderValue::from_static("198.51.100.1"));
    assert_eq!(
      ClientIp::from_request_parts(&mut parts, &()).await,
      Ok(ClientIp(ip("10.0.0.2")))
    );
    parts
      .extensions
      .insert(TrustedProxies::new(["10.0.0.2"]).unwrap());
    assert_eq!(
      ClientIp::from_request_parts(&mut parts, &()).await,
      Ok(ClientIp(ip("198.51.100.1")))
    );
  }
}
