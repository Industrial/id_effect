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
//! use a lenient one such as `authenticated()`. The guard is required either way.
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
