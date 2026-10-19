//! The startup security check (ADR 0013 R10): named probes over what the app states about itself
//! and what the [posture registry](id_effect::observability::posture) recorded.
//!
//! Every weakening of a secure default is a named `dangerous_*` / `insecure_*` opt-out that
//! records itself in the registry. [`Posture::check`] turns the registry, the app's own
//! statements (its console, `/docs`, mesh transport, CSRF layer, in-flight cap) and the
//! operator's accepted risks into a [`PostureReport`] with one [`Probe`] per rule:
//!
//! | Probe | Fails when |
//! |---|---|
//! | [`CONSOLE_GUARDED`] | the app states its console is [`Surface::Open`] |
//! | [`DOCS_CLOSED`] | `/docs` is [`Surface::Open`], or `OpenApiUi::public_dangerous` was taken |
//! | [`MESH_ENCRYPTED`] | the mesh transport is plaintext, or an `id_effect_node` opt-out was taken (plaintext TCP, unverified servers or clients, a short cookie) |
//! | [`REDACTION_ON`] | the log redaction policy is empty, or `RedactionPolicy::dangerous_none` or `dangerous_empty` was taken |
//! | [`SSRF_ON`] | an `id_effect_platform::http` opt-out (private networks, an unguarded client) or an `id_effect_auth` `insecure_allow_http` (OIDC, JWKS) was taken |
//! | [`CSRF_ON`] | the app states CSRF protection is off, or `SecurityLayer::dangerous_without_csrf` was taken |
//! | [`LOAD_SHEDDING_ON`] | the server has no in-flight cap, `HostConfig::dangerous_no_load_shedding` or a `ConnectionLimits::dangerous_*` opt-out was taken |
//! | [`NO_MEMORY_ONLY_STATE`] | security state that must be shared lives in one process (an id ending in `::memory_only`, e.g. `MemoryTotpStepStore`) |
//! | [`NO_UNACCEPTED_OPT_OUT`] | any recorded opt-out is not in `security.accepted_risks` with a reason |
//!
//! An opt-out the operator accepted — listed under [`ACCEPTED_RISKS_KEY`] in the profile's
//! configuration, with a non-empty reason — turns the probes it touches into
//! [`ProbeStatus::Accepted`] instead of [`ProbeStatus::Fail`]; listed without a reason it counts
//! as not accepted. A probe about a surface the app does not have (no console, no mesh) is
//! [`ProbeStatus::NotApplicable`].
//!
//! [`PostureReport::enforce`] is the startup gate: under the `prod` profile (any profile but
//! `dev` and `test`, see [`Enforcement::for_profile`]) a failed probe is an error and the
//! process should not start; `dev` and `test` report and start.
//!
//! ```rust
//! use id_effect_kit::posture::{AcceptedRisks, Posture, ProbeStatus, Surface, NO_UNACCEPTED_OPT_OUT};
//!
//! // What the app states about itself; `evaluate` takes the opt-outs explicitly, `check` reads
//! // the process's posture registry.
//! let app = Posture::new("prod").console(Surface::Guarded).csrf(true).max_in_flight(Some(512));
//! let clean = app.evaluate(&[]);
//! assert!(clean.passed(), "{clean}");
//!
//! let opt_out = [("demo::Client::dangerous_skip_check", "reopens the demo finding")];
//! let refused = app.evaluate(&opt_out);
//! assert_eq!(refused.probe(NO_UNACCEPTED_OPT_OUT).unwrap().status, ProbeStatus::Fail);
//! assert!(refused.enforce().is_err());
//!
//! let accepted = app
//!   .clone()
//!   .accepted_risks(AcceptedRisks::new().accept(opt_out[0].0, "reviewed 2026-10: test fixture only"))
//!   .evaluate(&opt_out);
//! assert!(accepted.passed(), "{accepted}");
//! ```
//!
//! In the profile's TOML (`config/prod.toml`), each accepted opt-out id maps to its reason:
//!
//! ```toml
//! [security.accepted_risks]
//! "id_effect_node::TcpTransport::dangerous_plaintext" = "mesh runs on a WireGuard overlay; reviewed 2026-10"
//! ```

use std::collections::BTreeMap;
use std::fmt;

use id_effect::observability::posture::dangerous_opt_outs;
use id_effect_config::{ConfigError, ProfiledConfig, Profiles};

/// The configuration key listing the accepted opt-outs: a table from opt-out id to the reason
/// it is accepted.
pub const ACCEPTED_RISKS_KEY: &str = "security.accepted_risks";

/// Probe: the operator console is guarded (or absent).
pub const CONSOLE_GUARDED: &str = "console guarded";
/// Probe: `/docs` and `/openapi.json` are closed or guarded (or absent).
pub const DOCS_CLOSED: &str = "docs closed";
/// Probe: the mesh transport is encrypted and mutually authenticated (or there is no mesh).
pub const MESH_ENCRYPTED: &str = "mesh transport encrypted";
/// Probe: the log redaction policy redacts something.
pub const REDACTION_ON: &str = "redaction policy non-empty";
/// Probe: outbound HTTP keeps the SSRF guard.
pub const SSRF_ON: &str = "SSRF guard on";
/// Probe: CSRF protection is on (or the app serves no browser).
pub const CSRF_ON: &str = "CSRF on";
/// Probe: the server caps requests in flight.
pub const LOAD_SHEDDING_ON: &str = "load shedding on";
/// Probe: security state that must be shared across instances is not memory-only.
pub const NO_MEMORY_ONLY_STATE: &str = "no memory-only security state";
/// Probe: every recorded opt-out is an accepted risk with a reason.
pub const NO_UNACCEPTED_OPT_OUT: &str = "no unaccepted opt-out";

/// Every probe, in report order.
pub const PROBES: [&str; 9] = [
  CONSOLE_GUARDED,
  DOCS_CLOSED,
  MESH_ENCRYPTED,
  REDACTION_ON,
  SSRF_ON,
  CSRF_ON,
  LOAD_SHEDDING_ON,
  NO_MEMORY_ONLY_STATE,
  NO_UNACCEPTED_OPT_OUT,
];

/// What a failed probe does to startup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Enforcement {
  /// Report and start (`dev`, `test`).
  Report,
  /// Refuse to start ([`PostureReport::enforce`] is an error).
  Refuse,
}

impl Enforcement {
  /// [`Report`](Self::Report) for the `dev` and `test` profiles, [`Refuse`](Self::Refuse) for
  /// every other (`prod`, `staging`, …): an unknown profile is treated as production.
  pub fn for_profile(profile: &str) -> Self {
    match profile.trim() {
      "dev" | "test" => Self::Report,
      _ => Self::Refuse,
    }
  }
}

/// How an operator surface (the console, `/docs`) is served.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Surface {
  /// Not mounted at all in this profile.
  Absent,
  /// Mounted behind a guard (authentication, a role, a token).
  Guarded,
  /// Mounted for anyone who can reach it.
  Open,
}

/// The verdict of one [`Probe`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeStatus {
  /// The rule holds.
  Pass,
  /// The rule is broken only by opt-outs the operator accepted with a reason.
  Accepted,
  /// The app has no such surface (no console, no mesh, no browser).
  NotApplicable,
  /// The rule is broken.
  Fail,
}

impl ProbeStatus {
  fn label(self) -> &'static str {
    match self {
      Self::Pass => "pass",
      Self::Accepted => "accepted",
      Self::NotApplicable => "n/a",
      Self::Fail => "FAIL",
    }
  }
}

/// One named check of a [`PostureReport`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Probe {
  /// One of [`PROBES`].
  pub name: &'static str,
  /// The verdict.
  pub status: ProbeStatus,
  /// Why, in one line.
  pub detail: String,
}

/// A recorded opt-out and whether the operator accepted it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OptOut {
  /// The opt-out's id (its API path, `id_effect_node::TcpTransport::dangerous_plaintext`).
  pub id: String,
  /// The finding or risk it reopens.
  pub reopens: String,
  /// The reason from `security.accepted_risks`, when it is listed there with one.
  pub accepted: Option<String>,
}

/// The opt-outs the operator accepts: opt-out id → reason (from [`ACCEPTED_RISKS_KEY`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AcceptedRisks(BTreeMap<String, String>);

impl AcceptedRisks {
  /// No accepted risk.
  pub fn new() -> Self {
    Self::default()
  }

  /// Accept the opt-out `id` for `reason`. A blank reason does not accept it: the probes treat
  /// it as listed without a reason.
  pub fn accept(mut self, id: impl Into<String>, reason: impl Into<String>) -> Self {
    self.0.insert(id.into(), reason.into());
    self
  }

  /// The table under [`ACCEPTED_RISKS_KEY`] in `config` (none when the key is absent). A value
  /// that is not a table of strings is a [`ConfigError`].
  // `ConfigError` is the configuration crate's error, returned as it is elsewhere.
  #[allow(clippy::result_large_err)]
  pub fn from_config(config: &ProfiledConfig) -> Result<Self, ConfigError> {
    let figment = config.figment();
    if !figment.contains(ACCEPTED_RISKS_KEY) {
      return Ok(Self::new());
    }
    figment
      .extract_inner::<BTreeMap<String, String>>(ACCEPTED_RISKS_KEY)
      .map(Self)
      .map_err(ConfigError::from)
  }

  /// The non-blank reason `id` is accepted for, if any.
  pub fn reason(&self, id: &str) -> Option<&str> {
    self
      .0
      .get(id)
      .map(|reason| reason.trim())
      .filter(|reason| !reason.is_empty())
  }

  /// Whether `id` is listed, with or without a reason.
  pub fn lists(&self, id: &str) -> bool {
    self.0.contains_key(id)
  }

  /// The listed ids and reasons, sorted by id.
  pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
    self
      .0
      .iter()
      .map(|(id, reason)| (id.as_str(), reason.as_str()))
  }
}

/// What the app states about one probe.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Stated {
  Pass(String),
  Fail(String),
  Absent(String),
}

/// The inputs of the startup check: the profile, the accepted risks and what the app states
/// about its surfaces. Build it, then [`check`](Self::check) (the registry) or
/// [`evaluate`](Self::evaluate) (a given opt-out list).
///
/// Probes the app states nothing about are judged from the registry alone: redaction, SSRF,
/// load shedding and memory-only state pass when no opt-out was recorded (their defaults are on);
/// the console, `/docs`, the mesh and CSRF are [`ProbeStatus::NotApplicable`].
#[derive(Clone, Debug)]
#[must_use = "a posture does nothing until `check()`"]
pub struct Posture {
  profile: String,
  accepted: AcceptedRisks,
  console: Option<Stated>,
  docs: Option<Stated>,
  mesh: Option<Stated>,
  redaction: Option<Stated>,
  csrf: Option<Stated>,
  load_shedding: Option<Stated>,
}

impl Posture {
  /// The check for `profile` (`dev`, `test`, `prod`, …), with no accepted risk and nothing
  /// stated yet.
  pub fn new(profile: impl AsRef<str>) -> Self {
    Self {
      profile: profile.as_ref().trim().to_owned(),
      accepted: AcceptedRisks::new(),
      console: None,
      docs: None,
      mesh: None,
      redaction: None,
      csrf: None,
      load_shedding: None,
    }
  }

  /// The check for the profile `profiles` selects (`--profile`, `ID_EFFECT_PROFILE`, default
  /// `dev`), with the accepted risks from that profile's configuration
  /// ([`AcceptedRisks::from_config`]).
  #[allow(clippy::result_large_err)]
  pub fn load(profiles: &Profiles) -> Result<Self, ConfigError> {
    let config = profiles.load()?;
    let accepted = AcceptedRisks::from_config(&config)?;
    Ok(Self::new(config.profile().as_str()).accepted_risks(accepted))
  }

  /// Replace the accepted risks.
  pub fn accepted_risks(mut self, accepted: AcceptedRisks) -> Self {
    self.accepted = accepted;
    self
  }

  /// How the operator console is served ([`CONSOLE_GUARDED`]).
  pub fn console(mut self, surface: Surface) -> Self {
    self.console = Some(surface_stated("the console", surface));
    self
  }

  /// How `/docs` and `/openapi.json` are served in this profile ([`DOCS_CLOSED`]).
  pub fn docs(mut self, surface: Surface) -> Self {
    self.docs = Some(surface_stated("/docs", surface));
    self
  }

  /// Whether the mesh transport encrypts and mutually authenticates ([`MESH_ENCRYPTED`]): the
  /// QUIC transport always does; for TCP see [`tcp_transport`](Self::tcp_transport).
  pub fn mesh_encrypted(mut self, encrypted: bool) -> Self {
    self.mesh = Some(if encrypted {
      Stated::Pass("the mesh transport is TLS with mutual authentication".to_owned())
    } else {
      Stated::Fail("the mesh transport is plaintext".to_owned())
    });
    self
  }

  /// The app has no mesh ([`MESH_ENCRYPTED`] is not applicable).
  pub fn no_mesh(mut self) -> Self {
    self.mesh = Some(Stated::Absent("no mesh transport".to_owned()));
    self
  }

  /// [`mesh_encrypted`](Self::mesh_encrypted) from `transport` (`TcpTransport::is_encrypted`).
  #[cfg(feature = "cluster")]
  pub fn tcp_transport(self, transport: &id_effect_node::TcpTransport) -> Self {
    self.mesh_encrypted(transport.is_encrypted())
  }

  /// Whether the log redaction policy redacts something ([`REDACTION_ON`]).
  pub fn redaction_non_empty(mut self, non_empty: bool) -> Self {
    self.redaction = Some(if non_empty {
      Stated::Pass("the log redaction policy redacts secret fields".to_owned())
    } else {
      Stated::Fail("the log redaction policy is empty: secrets reach every log backend".to_owned())
    });
    self
  }

  /// [`redaction_non_empty`](Self::redaction_non_empty) from `policy`.
  #[cfg(feature = "logger")]
  pub fn redaction(self, policy: &id_effect_logger::RedactionPolicy) -> Self {
    self.redaction_non_empty(!policy.is_empty())
  }

  /// Whether the app's `SecurityLayer` checks CSRF tokens ([`CSRF_ON`]): `true` for
  /// `SecurityLayer::recommended`, `false` after `dangerous_without_csrf`. With the `web`
  /// feature, [`security_layer`](Self::security_layer) reads it from the layer itself.
  pub fn csrf(mut self, on: bool) -> Self {
    self.csrf = Some(if on {
      Stated::Pass("SecurityLayer checks a CSRF token on every state-changing request".to_owned())
    } else {
      Stated::Fail("CSRF protection is off".to_owned())
    });
    self
  }

  /// [`csrf`](Self::csrf) from the app's `SecurityLayer` (`SecurityLayer::csrf_enabled`).
  #[cfg(feature = "web")]
  pub fn security_layer(self, layer: &id_effect_axum::server::security::SecurityLayer) -> Self {
    self.csrf(layer.csrf_enabled())
  }

  /// The app serves no browser: no cookies, no forms ([`CSRF_ON`] is not applicable).
  pub fn no_browser(mut self) -> Self {
    self.csrf = Some(Stated::Absent("no browser-facing routes".to_owned()));
    self
  }

  /// The server's in-flight request cap ([`LOAD_SHEDDING_ON`]); `None` is no cap.
  pub fn max_in_flight(mut self, cap: Option<usize>) -> Self {
    self.load_shedding = Some(match cap {
      Some(n) => Stated::Pass(format!(
        "at most {n} requests in flight, the rest answered 503"
      )),
      None => Stated::Fail("no in-flight request cap".to_owned()),
    });
    self
  }

  /// [`max_in_flight`](Self::max_in_flight) from the server's `HostConfig`.
  #[cfg(feature = "web")]
  pub fn host(self, config: &id_effect_axum::HostConfig) -> Self {
    self.max_in_flight(config.load_shed.max_in_flight())
  }

  /// The profile checked.
  pub fn profile(&self) -> &str {
    &self.profile
  }

  /// The report against the opt-outs recorded in this process's posture registry
  /// ([`dangerous_opt_outs`]). Call it once the app is assembled, so every opt-out it takes has
  /// been recorded.
  pub fn check(&self) -> PostureReport {
    self.evaluate(&dangerous_opt_outs())
  }

  /// The report against `opt_outs` (`(id, reopens)` pairs, as the registry lists them).
  pub fn evaluate(&self, opt_outs: &[(&str, &str)]) -> PostureReport {
    let opt_outs: Vec<OptOut> = opt_outs
      .iter()
      .map(|(id, reopens)| OptOut {
        id: (*id).to_owned(),
        reopens: (*reopens).to_owned(),
        accepted: self.accepted.reason(id).map(str::to_owned),
      })
      .collect();
    let stated = [
      (CONSOLE_GUARDED, &self.console),
      (DOCS_CLOSED, &self.docs),
      (MESH_ENCRYPTED, &self.mesh),
      (REDACTION_ON, &self.redaction),
      (SSRF_ON, &None),
      (CSRF_ON, &self.csrf),
      (LOAD_SHEDDING_ON, &self.load_shedding),
      (NO_MEMORY_ONLY_STATE, &None),
    ];
    let mut probes: Vec<Probe> = stated
      .into_iter()
      .map(|(name, stated)| self.probe(name, stated.as_ref(), &opt_outs))
      .collect();
    probes.push(self.unaccepted_probe(&opt_outs));
    PostureReport {
      profile: self.profile.clone(),
      enforcement: Enforcement::for_profile(&self.profile),
      probes,
      opt_outs,
    }
  }

  fn probe(&self, name: &'static str, stated: Option<&Stated>, opt_outs: &[OptOut]) -> Probe {
    let related: Vec<&OptOut> = opt_outs.iter().filter(|o| covers(name, &o.id)).collect();
    let unaccepted: Vec<&str> = related
      .iter()
      .filter(|o| o.accepted.is_none())
      .map(|o| o.id.as_str())
      .collect();
    let (status, detail) = match stated {
      Some(Stated::Fail(why)) if unaccepted.is_empty() => (ProbeStatus::Fail, why.clone()),
      Some(Stated::Fail(why)) => (
        ProbeStatus::Fail,
        format!("{why}; opt-out not accepted: {}", unaccepted.join(", ")),
      ),
      _ if !unaccepted.is_empty() => (
        ProbeStatus::Fail,
        format!("opt-out not accepted: {}", unaccepted.join(", ")),
      ),
      _ if !related.is_empty() => (
        ProbeStatus::Accepted,
        format!("accepted risk: {}", ids(&related)),
      ),
      Some(Stated::Pass(why)) => (ProbeStatus::Pass, why.clone()),
      Some(Stated::Absent(why)) => (ProbeStatus::NotApplicable, why.clone()),
      None => default_verdict(name),
    };
    Probe {
      name,
      status,
      detail,
    }
  }

  fn unaccepted_probe(&self, opt_outs: &[OptOut]) -> Probe {
    let unaccepted: Vec<String> = opt_outs
      .iter()
      .filter(|o| o.accepted.is_none())
      .map(|o| {
        if self.accepted.lists(&o.id) {
          format!("{} (listed in {ACCEPTED_RISKS_KEY} without a reason)", o.id)
        } else {
          o.id.clone()
        }
      })
      .collect();
    let (status, detail) = if !unaccepted.is_empty() {
      (
        ProbeStatus::Fail,
        format!(
          "not in {ACCEPTED_RISKS_KEY} with a reason: {}",
          unaccepted.join(", ")
        ),
      )
    } else if opt_outs.is_empty() {
      (ProbeStatus::Pass, "no opt-out recorded".to_owned())
    } else {
      let all: Vec<&OptOut> = opt_outs.iter().collect();
      (
        ProbeStatus::Accepted,
        format!("every opt-out is an accepted risk: {}", ids(&all)),
      )
    };
    Probe {
      name: NO_UNACCEPTED_OPT_OUT,
      status,
      detail,
    }
  }
}

fn surface_stated(what: &str, surface: Surface) -> Stated {
  match surface {
    Surface::Absent => Stated::Absent(format!("{what} is not mounted")),
    Surface::Guarded => Stated::Pass(format!("{what} is behind a guard")),
    Surface::Open => Stated::Fail(format!("{what} is served to anyone who can reach it")),
  }
}

fn ids(opt_outs: &[&OptOut]) -> String {
  opt_outs
    .iter()
    .map(|o| o.id.as_str())
    .collect::<Vec<_>>()
    .join(", ")
}

/// The verdict of a probe the app stated nothing about and no opt-out touches.
fn default_verdict(name: &str) -> (ProbeStatus, String) {
  match name {
    CONSOLE_GUARDED => (ProbeStatus::NotApplicable, "no console stated".to_owned()),
    DOCS_CLOSED => (ProbeStatus::NotApplicable, "no /docs stated".to_owned()),
    MESH_ENCRYPTED => (
      ProbeStatus::NotApplicable,
      "no mesh transport stated".to_owned(),
    ),
    CSRF_ON => (
      ProbeStatus::NotApplicable,
      "no browser surface stated".to_owned(),
    ),
    _ => (
      ProbeStatus::Pass,
      "no opt-out recorded; the default is on".to_owned(),
    ),
  }
}

/// Whether the opt-out `id` breaks the rule of the probe `name`.
fn covers(name: &str, id: &str) -> bool {
  match name {
    CONSOLE_GUARDED => id.starts_with("id_effect_console::"),
    DOCS_CLOSED => id.starts_with("id_effect_axum::openapi::"),
    MESH_ENCRYPTED => id.starts_with("id_effect_node::"),
    REDACTION_ON => id.starts_with("id_effect_logger::RedactionPolicy::"),
    SSRF_ON => {
      id.starts_with("id_effect_platform::http::")
        || (id.starts_with("id_effect_auth::") && id.ends_with("insecure_allow_http"))
    }
    CSRF_ON => id.to_ascii_lowercase().contains("csrf"),
    LOAD_SHEDDING_ON => {
      id == "id_effect_axum::HostConfig::dangerous_no_load_shedding"
        || id.starts_with("id_effect_axum::server::ConnectionLimits::")
    }
    NO_MEMORY_ONLY_STATE => id.ends_with("::memory_only"),
    _ => true,
  }
}

/// The result of [`Posture::check`]: one [`Probe`] per entry of [`PROBES`], in that order, and
/// every recorded opt-out. `Display` renders it for the startup log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PostureReport {
  profile: String,
  enforcement: Enforcement,
  probes: Vec<Probe>,
  opt_outs: Vec<OptOut>,
}

impl PostureReport {
  /// The profile checked.
  pub fn profile(&self) -> &str {
    &self.profile
  }

  /// What a failed probe does under this profile.
  pub fn enforcement(&self) -> Enforcement {
    self.enforcement
  }

  /// Every probe, in [`PROBES`] order.
  pub fn probes(&self) -> &[Probe] {
    &self.probes
  }

  /// The probe named `name`.
  pub fn probe(&self, name: &str) -> Option<&Probe> {
    self.probes.iter().find(|p| p.name == name)
  }

  /// Every recorded opt-out, sorted by id.
  pub fn opt_outs(&self) -> &[OptOut] {
    &self.opt_outs
  }

  /// The failed probes.
  pub fn failures(&self) -> impl Iterator<Item = &Probe> {
    self.probes.iter().filter(|p| p.status == ProbeStatus::Fail)
  }

  /// Whether no probe failed.
  pub fn passed(&self) -> bool {
    self.failures().next().is_none()
  }

  /// The startup gate: `Err` when a probe failed and the profile [refuses](Enforcement::Refuse)
  /// to start; otherwise the report (log it).
  pub fn enforce(self) -> Result<Self, PostureRefused> {
    if self.enforcement == Enforcement::Refuse && !self.passed() {
      Err(PostureRefused(Box::new(self)))
    } else {
      Ok(self)
    }
  }
}

impl fmt::Display for PostureReport {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let gate = match self.enforcement {
      Enforcement::Report => "reports only",
      Enforcement::Refuse => "refuses to start on a failed probe",
    };
    writeln!(f, "security posture, profile {} ({gate}):", self.profile)?;
    for probe in &self.probes {
      writeln!(
        f,
        "  {:<8} {}: {}",
        probe.status.label(),
        probe.name,
        probe.detail
      )?;
    }
    for opt_out in &self.opt_outs {
      match &opt_out.accepted {
        Some(reason) => writeln!(
          f,
          "  opt-out  {} (accepted: {reason}): {}",
          opt_out.id, opt_out.reopens
        )?,
        None => writeln!(
          f,
          "  opt-out  {} (not accepted): {}",
          opt_out.id, opt_out.reopens
        )?,
      }
    }
    Ok(())
  }
}

/// [`PostureReport::enforce`] under a profile that refuses to start: the report, with at least
/// one failed probe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PostureRefused(pub Box<PostureReport>);

impl fmt::Display for PostureRefused {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let failed: Vec<&str> = self.0.failures().map(|p| p.name).collect();
    write!(
      f,
      "security posture: profile {} refuses to start, failed: {}\n{}",
      self.0.profile,
      failed.join(", "),
      self.0
    )
  }
}

impl std::error::Error for PostureRefused {}

/// [`Posture::new`]`(profile).`[`check`](Posture::check)`()`: the report for `profile` from the
/// posture registry alone, with nothing stated and no accepted risk.
pub fn check(profile: impl AsRef<str>) -> PostureReport {
  Posture::new(profile).check()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn every_probe_covers_its_own_ids_only() {
    let cases = [
      (
        DOCS_CLOSED,
        "id_effect_axum::openapi::OpenApiUi::public_dangerous",
      ),
      (
        MESH_ENCRYPTED,
        "id_effect_node::TcpTransport::dangerous_plaintext",
      ),
      (
        REDACTION_ON,
        "id_effect_logger::RedactionPolicy::dangerous_none",
      ),
      (
        SSRF_ON,
        "id_effect_auth::oidc::OidcConfig::insecure_allow_http",
      ),
      (
        LOAD_SHEDDING_ON,
        "id_effect_axum::HostConfig::dangerous_no_load_shedding",
      ),
      (
        NO_MEMORY_ONLY_STATE,
        "id_effect_auth::MemoryTotpStepStore::memory_only",
      ),
    ];
    for (probe, id) in cases {
      for other in PROBES {
        let expected = other == probe || other == NO_UNACCEPTED_OPT_OUT;
        assert_eq!(covers(other, id), expected, "{other} / {id}");
      }
    }
  }

  #[test]
  fn enforcement_treats_unknown_profiles_as_production() {
    assert_eq!(Enforcement::for_profile("dev"), Enforcement::Report);
    assert_eq!(Enforcement::for_profile(" test "), Enforcement::Report);
    assert_eq!(Enforcement::for_profile("prod"), Enforcement::Refuse);
    assert_eq!(Enforcement::for_profile("staging"), Enforcement::Refuse);
  }
}
