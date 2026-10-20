//! The operator console for id_effect services: live fibers, supervision trees, metrics, health
//! and the log level, behind an `id_effect_authz` guard.
//!
//! # The API
//!
//! [`Console`] collects what the console shows and [`Console::router`] turns it into an Axum
//! router answering JSON under [`API_PREFIX`] (`/_console/api`) and the dashboard page at
//! `/_console`:
//!
//! | Route | Answers |
//! |---|---|
//! | `GET fibers` | every live fiber ([`FiberView`], from core `fiber_dump`, feature `fiber-registry`) |
//! | `GET supervisors` | the registered supervision trees ([`SupervisorsView`], from `which_children`) |
//! | `GET metrics` | the metric registry and the runtime fiber gauges ([`MetricView`]) |
//! | `GET health` | the [`HealthRegistry`](id_effect_axum::HealthRegistry) report (always `200`) |
//! | `GET log-level` | the reloadable `EnvFilter` and the `EffectLogger` minimum ([`LogLevelView`]) |
//! | `PUT log-level` | changes both without a restart ([`LogLevelChange`]) |
//! | `GET jobs` | per-queue counts and the newest jobs of a job queue (`JobsView`, feature `jobs`) |
//! | `POST jobs/{id}/retry` | a `dead` or `cancelled` job runs again now (`JobView`; `404` unknown id, `409` other state) |
//! | `POST jobs/{id}/cancel` | a `scheduled` or `failed` job is cancelled (`JobView`; `404` unknown id, `409` other state) |
//!
//! The two `POST` routes require `Content-Type: application/json` (`415` otherwise), so a
//! cross-site form cannot trigger them.
//!
//! # The dashboard
//!
//! The same router serves `GET /_console`: one static page (inline CSS and vanilla JavaScript,
//! compiled in with `include_str!`, no build step and nothing loaded from another origin) that
//! polls the API and shows the health checks, the log level with a form that changes it, the
//! supervision trees, the job queues (when the `jobs` feature and a queue are configured, with a
//! retry button on dead and cancelled jobs and a cancel button on scheduled and failed ones), the
//! live fibers and the metrics. It is behind the same guard as the API.
//!
//! # Security model
//!
//! There is no unguarded constructor: the only way to get the routes is
//! [`Console::router`], which takes an authorization [`Guard`](id_effect_authz::Guard) and puts it
//! in front of every console route **and** the console router's fallback, so a principal the
//! guard denies learns nothing about the API (`401` anonymous, `403` otherwise, Problem Details
//! body). The authentication layer that sets the principal runs outside the guard
//! (`Router::layer` on the merged app), as for any `id_effect_authz` route. Console answers carry
//! `Cache-Control: no-store`.
//!
//! [`ConsoleConfig`] is where the console listens. Bound to a loopback address it serves the local
//! machine only; on any other address [`ConsoleConfig::requires_guard`] is true: the guard must then
//! restrict the console to operators (a role or permission policy), while a loopback console may
//! use a lenient one such as `authenticated()`. The guard is required either way. On the
//! console's own listener, [`ConsoleConfig::check_host_and_origin`] also answers `403` to a
//! request for another host or from another origin (DNS rebinding, cross-site posts; see
//! [Host and Origin](ConsoleConfig#host-and-origin)), by default allowing the bind address only.
//!
//! ```
//! use std::net::{Ipv6Addr, SocketAddr};
//!
//! use id_effect_authz::{Guard, require_role};
//! use id_effect_console::{Console, ConsoleConfig};
//!
//! let config = ConsoleConfig::new(SocketAddr::from((Ipv6Addr::LOCALHOST, 9090)));
//! assert!(!config.requires_guard());
//!
//! let console: axum::Router = Console::new().router(Guard::new("console", require_role("operator")));
//! # let _ = console;
//! ```
//!
//! Tier `platform` (ADR 0010 Amendment 6): the Ledger reference app under
//! `examples/reference/` depends on it.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod api;
#[cfg(feature = "jobs")]
mod jobs;
mod log_level;
mod supervisors;
mod ui;

pub use api::{API_PREFIX, Console, FiberView, MetricView};
#[cfg(feature = "jobs")]
pub use jobs::{JobView, JobsView, QueueView};
pub use log_level::{EnvFilterHandle, LogLevelChange, LogLevelView};
pub use supervisors::{SupervisorsView, UnreachableSupervisor};

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use axum::Router;
use axum::extract::Request;
use axum::http::{StatusCode, header};
use axum::middleware::Next;

/// Where the console listens, and the host names it answers to.
///
/// # Host and Origin
///
/// A console on loopback is still reachable from the operator's browser, so a page on another
/// site could point a DNS name at `127.0.0.1` (DNS rebinding) or post to it. The console's
/// listener therefore answers only requests whose `Host` (or HTTP/2 authority) is one of
/// [`allowed_hosts`](Self::allowed_hosts) and whose `Origin`, when sent, is `http(s)://` one of
/// them; anything else — no host, another host, `Origin: null` — is a **403** Problem Details
/// answer before any guard runs. Apply it with
/// [`check_host_and_origin`](Self::check_host_and_origin).
///
/// The allowed hosts default to the bind address (`127.0.0.1:9090`, and `localhost:9090` for a
/// loopback bind; a bind port of `0` allows any port). [`allow_hosts`](Self::allow_hosts)
/// replaces them, for a console reached through a tunnel or a proxy under another name. An entry
/// without a port allows that host on any port.
///
/// ```
/// use id_effect_console::ConsoleConfig;
///
/// let config = ConsoleConfig::loopback(9090);
/// assert_eq!(config.allowed_hosts(), ["127.0.0.1:9090", "localhost:9090"]);
/// assert!(config.allows_host("LOCALHOST:9090"));
/// assert!(!config.allows_host("rebind.attacker.example:9090"));
/// let tunnel = config.allow_hosts(["console.internal"]);
/// assert!(tunnel.allows_host("console.internal:8443"));
/// assert!(!tunnel.allows_host("127.0.0.1:9090"));
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ConsoleConfig {
  bind: SocketAddr,
  allowed_hosts: Vec<String>,
}

impl ConsoleConfig {
  /// Listens on `bind`, answering to the bind address.
  pub const fn new(bind: SocketAddr) -> Self {
    ConsoleConfig {
      bind,
      allowed_hosts: Vec::new(),
    }
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

  /// Answer to `hosts` (`host` or `host:port`, case-insensitive) instead of the bind address.
  /// Blank entries are skipped; an empty list restores the default.
  #[must_use]
  pub fn allow_hosts<H: Into<String>>(mut self, hosts: impl IntoIterator<Item = H>) -> Self {
    self.allowed_hosts = hosts
      .into_iter()
      .map(|host| host.into().trim().to_ascii_lowercase())
      .filter(|host| !host.is_empty())
      .collect();
    self
  }

  /// The hosts the console answers to: those of [`allow_hosts`](Self::allow_hosts), else the
  /// bind address (plus `localhost` for a loopback bind).
  pub fn allowed_hosts(&self) -> Vec<String> {
    if !self.allowed_hosts.is_empty() {
      return self.allowed_hosts.clone();
    }
    let ip = match self.bind.ip() {
      IpAddr::V4(ip) => ip.to_string(),
      IpAddr::V6(ip) => format!("[{ip}]"),
    };
    let mut names = vec![ip];
    if self.bind.ip().is_loopback() {
      names.push("localhost".to_owned());
    }
    match self.bind.port() {
      0 => names,
      port => names
        .into_iter()
        .map(|name| format!("{name}:{port}"))
        .collect(),
    }
  }

  /// Whether `authority` (a `Host` header value, `host` or `host:port`) is an allowed host.
  pub fn allows_host(&self, authority: &str) -> bool {
    host_allowed(&self.allowed_hosts(), authority)
  }

  /// `routes` (the console's, from [`Console::router`]) behind the Host and Origin check (see
  /// [Host and Origin](Self#host-and-origin)): put it on the console's listener.
  pub fn check_host_and_origin(&self, routes: Router) -> Router {
    let allowed: Arc<[String]> = self.allowed_hosts().into();
    routes.layer(axum::middleware::from_fn(
      move |request: Request, next: Next| {
        let allowed = Arc::clone(&allowed);
        async move {
          match foreign_request(&allowed, &request) {
            Some(detail) => api::problem(StatusCode::FORBIDDEN, detail),
            None => next.run(request).await,
          }
        }
      },
    ))
  }
}

/// Why `request` is not for the console (its host or origin is not allowed), or `None`.
fn foreign_request(allowed: &[String], request: &Request) -> Option<&'static str> {
  let host = request
    .headers()
    .get(header::HOST)
    .and_then(|v| v.to_str().ok())
    .map(str::to_owned)
    .or_else(|| request.uri().authority().map(|a| a.as_str().to_owned()));
  match host {
    Some(host) if host_allowed(allowed, &host) => {}
    _ => return Some("the console does not answer to this host"),
  }
  // No `Origin`: not a cross-origin browser request.
  let origin = request.headers().get(header::ORIGIN)?;
  let origin = origin.to_str().unwrap_or_default();
  let authority = origin
    .strip_prefix("https://")
    .or_else(|| origin.strip_prefix("http://"));
  match authority {
    Some(authority) if host_allowed(allowed, authority) => None,
    _ => Some("the console does not answer to this origin"),
  }
}

/// Whether `authority` matches one of `allowed` (an entry without a port matches any port).
fn host_allowed(allowed: &[String], authority: &str) -> bool {
  let (host, port) = split_authority(authority.trim());
  !host.is_empty()
    && allowed.iter().any(|entry| {
      let (allowed_host, allowed_port) = split_authority(entry);
      allowed_host.eq_ignore_ascii_case(host) && allowed_port.is_none_or(|p| Some(p) == port)
    })
}

/// `host[:port]` or `[v6][:port]` into the host and the port, if any.
fn split_authority(authority: &str) -> (&str, Option<&str>) {
  if authority.starts_with('[') {
    return match authority.find(']') {
      Some(end) => {
        let port = authority[end + 1..].strip_prefix(':');
        (&authority[..=end], port)
      }
      None => (authority, None),
    };
  }
  match authority.rsplit_once(':') {
    Some((host, port)) if !host.contains(':') => (host, Some(port)),
    _ => (authority, None),
  }
}
