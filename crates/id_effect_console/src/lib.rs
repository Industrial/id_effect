//! The operator console for id_effect services: live fibers, jobs and queues, behind
//! `id_effect_authz` guards.
//!
//! [`ConsoleConfig`] is where the console listens. Bound to a loopback address it serves the local
//! machine only; on any other address [`ConsoleConfig::requires_guard`] is true: the console must
//! not serve that address without an authorization guard.
//!
//! ```
//! use std::net::{Ipv6Addr, SocketAddr};
//!
//! use id_effect_console::ConsoleConfig;
//!
//! let config = ConsoleConfig::new(SocketAddr::from((Ipv6Addr::LOCALHOST, 9090)));
//! assert!(!config.requires_guard());
//! ```
//!
//! Tier `integration` (ADR 0010 Amendment 6).

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

/// Where the console listens.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ConsoleConfig {
  bind: SocketAddr,
}

impl ConsoleConfig {
  /// Listens on `bind`.
  pub const fn new(bind: SocketAddr) -> Self {
    ConsoleConfig { bind }
  }

  /// Listens on `127.0.0.1:port`.
  pub const fn loopback(port: u16) -> Self {
    ConsoleConfig::new(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port))
  }

  /// The address the console listens on.
  pub const fn bind(&self) -> SocketAddr {
    self.bind
  }

  /// Whether the console is reachable from other machines, so it needs an authorization guard.
  pub fn requires_guard(&self) -> bool {
    !self.bind.ip().is_loopback()
  }
}
