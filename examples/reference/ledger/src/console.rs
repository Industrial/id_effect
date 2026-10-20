//! The operator console at `/_console` (id_effect_console): the live fibers, the supervision
//! trees, the metrics (the database and HTTP series included), the health checks, the log level
//! (read and changed at runtime) and the job queues, as JSON under `/_console/api/` and a
//! dashboard page.
//!
//! Every console route, the page and the API's fallback, is behind
//! `Guard::named(&policies, "console.admin")`: an `operator` who passed the TOTP step
//! ([`crate::policy`]). It is served on the application's listener, inside its authentication
//! layer, so an operator signs in like anyone else; an anonymous caller gets `401` and any other
//! user `403`, and neither learns which console routes exist. A `PUT log-level` from a session
//! needs the CSRF header like every other unsafe request.
//!
//! Being on the main listener, it needs no `ConsoleConfig::check_host_and_origin` (that check is
//! for a console on a loopback listener of its own, which trusts the connection): here a caller
//! is authenticated by its session, and a cross-site request is stopped by the CSRF check.
//! `console.enabled` mounts it in `dev` and the tests only; in `prod` it is absent, and the
//! startup posture check reports it so (`Surface::Absent`, [`crate::app::posture`]).

use id_effect_kit::authz::{Guard, Policies};
use id_effect_kit::axum::HealthRegistry;
use id_effect_kit::console::{Console, EnvFilterHandle};
use id_effect_kit::jobs::PgJobQueue;

/// The console over `health`, the job queue `jobs` and, when the OpenTelemetry starter is
/// installed, its reloadable log filter.
pub fn router(
  policies: &Policies,
  health: HealthRegistry,
  jobs: PgJobQueue,
  env_filter: Option<EnvFilterHandle>,
) -> axum::Router {
  let console = Console::new().health(health).jobs(jobs);
  let console = match env_filter {
    Some(handle) => console.env_filter(handle),
    None => console,
  };
  console.router(Guard::named(policies, "console.admin"))
}
