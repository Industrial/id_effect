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
//! | [`DOCS_CLOSED`] | `/docs` is [`Surface::Open`], or `OpenApiUi::public_dangerous` or `ContractDocs::public_dangerous` (the RPC documents, C12) was taken |
//! | [`MESH_ENCRYPTED`] | the mesh transport is plaintext, or an `id_effect_node` opt-out was taken (plaintext TCP, unverified servers or clients, a short cookie) |
//! | [`REDACTION_ON`] | the log redaction policy is empty, or `RedactionPolicy::dangerous_none` or `dangerous_empty` was taken |
//! | [`SSRF_ON`] | an `id_effect_platform::http` opt-out (private networks, an unguarded client) or an `id_effect_auth` `insecure_allow_http` (OIDC, JWKS) was taken |
//! | [`CSRF_ON`] | the app states CSRF protection is off, or `SecurityLayer::dangerous_without_csrf` was taken |
//! | [`LOAD_SHEDDING_ON`] | the server has no in-flight cap, `HostConfig::dangerous_no_load_shedding`, a `ConnectionLimits::dangerous_*` opt-out or `QuicEndpointConfig::dangerous_raise_limits` was taken |
//! | [`NO_MEMORY_ONLY_STATE`] | security state that must be shared lives in one process (an id ending in `::memory_only`, e.g. `MemoryTotpStepStore`, `MemoryThrottleStore`) |
//! | [`PROCEDURES_DECLARED`] | a stated RPC procedure or route has no access decision (C1: `route_unchecked`), or `channels::dangerous_allow_all_joins` was taken |
//! | [`REALTIME_ORIGINS_SET`] | a stated WebSocket / WebTransport origin allowlist admits any origin or is empty, or `OriginPolicy::dangerous_any_origin` was taken (C4) |
//! | [`ZERO_RTT_OFF`] | the app states HTTP/3 0-RTT is admitted, or `Http3Options::dangerous_allow_zero_rtt_for_queries` was taken (C7) |
//! | [`QUIC_RETRY_ON`] | the app states QUIC address validation is off, or `QuicEndpointConfig::dangerous_no_retry` was taken (C6) |
//! | [`HTTP3_WITH_TLS`] | the host enables HTTP/3 without a TLS certificate (`HostConfig::posture_warnings`) |
//! | [`NO_DEV_CERTIFICATE`] | outside `dev` and `test`, the served certificate is the `id-effect dev` certificate ([`is_dev_certificate`], C13) |
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
/// Probe: every RPC procedure (and typed route) declares public or guarded (ADR 0014 C1).
pub const PROCEDURES_DECLARED: &str = "every procedure declared";
/// Probe: WebSocket and WebTransport upgrades check `Origin` against a non-empty allowlist (C4).
pub const REALTIME_ORIGINS_SET: &str = "realtime origins configured";
/// Probe: HTTP/3 0-RTT early data is refused (C7).
pub const ZERO_RTT_OFF: &str = "0-RTT off";
/// Probe: the QUIC endpoint validates client addresses with Retry (C6).
pub const QUIC_RETRY_ON: &str = "quic retry on";
/// Probe: HTTP/3 is served only with a TLS certificate.
pub const HTTP3_WITH_TLS: &str = "HTTP/3 only with TLS";
/// Probe: outside `dev` and `test`, the served certificate is not the `id-effect dev` one (C13).
pub const NO_DEV_CERTIFICATE: &str = "no dev certificate";
/// Probe: every recorded opt-out is an accepted risk with a reason.
pub const NO_UNACCEPTED_OPT_OUT: &str = "no unaccepted opt-out";

/// Every probe, in report order.
pub const PROBES: [&str; 15] = [
  CONSOLE_GUARDED,
  DOCS_CLOSED,
  MESH_ENCRYPTED,
  REDACTION_ON,
  SSRF_ON,
  CSRF_ON,
  LOAD_SHEDDING_ON,
  NO_MEMORY_ONLY_STATE,
  PROCEDURES_DECLARED,
  REALTIME_ORIGINS_SET,
  ZERO_RTT_OFF,
  QUIC_RETRY_ON,
  HTTP3_WITH_TLS,
  NO_DEV_CERTIFICATE,
  NO_UNACCEPTED_OPT_OUT,
];

/// The organization (`O=`) of the subject and issuer of every certificate `id-effect dev` issues
/// (its CA and its localhost certificate): what [`is_dev_certificate`] looks for.
pub const DEV_CERTIFICATE_ORGANIZATION: &str =
  "id-effect development certificate (never for production)";

/// Whether the DER certificate `der` was issued by `id-effect dev` (its subject or issuer names
/// [`DEV_CERTIFICATE_ORGANIZATION`]). The prod profile refuses to serve one ([`NO_DEV_CERTIFICATE`],
/// ADR 0014 C13).
pub fn is_dev_certificate(der: &[u8]) -> bool {
  let marker = DEV_CERTIFICATE_ORGANIZATION.as_bytes();
  der.windows(marker.len()).any(|window| window == marker)
}

/// The access a served procedure or route declares ([`Posture::procedures`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeclaredAccess {
  /// Anyone may call it (`#[public]`, `.public()`).
  Public,
  /// Guards decide first (`#[guard(G)]`, `.guard(g)`).
  Guarded,
  /// No decision: served to every caller (the deprecated `route_unchecked`).
  Undeclared,
}

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
  procedures: Option<Vec<(String, DeclaredAccess)>>,
  origins: Option<Vec<OriginStatement>>,
  zero_rtt: Option<Stated>,
  quic_retry: Option<Stated>,
  http3: Option<Stated>,
  certificate: Option<Stated>,
}

/// One origin allowlist the app states ([`Posture::realtime_origins`]).
#[derive(Clone, Debug, PartialEq, Eq)]
struct OriginStatement {
  what: String,
  any: bool,
  allowed: usize,
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
      procedures: None,
      origins: None,
      zero_rtt: None,
      quic_retry: None,
      http3: None,
      certificate: None,
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

  /// What the server's `HostConfig` states: its in-flight cap ([`max_in_flight`](Self::max_in_flight));
  /// with feature `http3`, also whether HTTP/3 has a certificate ([`HTTP3_WITH_TLS`]) and which
  /// certificate the host serves ([`NO_DEV_CERTIFICATE`]; PEM files are read now).
  #[cfg(feature = "web")]
  pub fn host(self, config: &id_effect_axum::HostConfig) -> Self {
    let posture = self.max_in_flight(config.load_shed.max_in_flight());
    #[cfg(feature = "http3")]
    let posture = posture
      .http3(config.http3, config.tls.is_some())
      .host_certificate(config.tls.as_ref());
    posture
  }

  /// The certificate of `tls` ([`NO_DEV_CERTIFICATE`]): the leaf of the PEM chain, or of the
  /// provided settings' identity. A resolver without an identity (ACME) states nothing; a chain
  /// that cannot be read fails the probe.
  #[cfg(feature = "http3")]
  fn host_certificate(self, tls: Option<&id_effect_axum::server::HostTls>) -> Self {
    use id_effect_axum::server::{HostTls, ServerTls};
    let settings = match tls {
      None => return self,
      Some(HostTls::Files {
        cert_file,
        key_file,
        ..
      }) => ServerTls::from_pem_files(cert_file.clone(), key_file.clone(), None),
      Some(other) => other.server_tls(),
    };
    match settings {
      Ok(settings) => {
        let leaf = settings
          .identity()
          .and_then(|identity| identity.current().cert.first().cloned());
        match leaf {
          Some(leaf) => self.tls_certificate(leaf.as_ref()),
          None => self,
        }
      }
      Err(e) => self.certificate_unreadable(&e.to_string()),
    }
  }

  #[cfg(feature = "http3")]
  fn certificate_unreadable(mut self, why: &str) -> Self {
    self.certificate = Some(Stated::Fail(format!(
      "the TLS certificate cannot be read: {why}"
    )));
    self
  }

  /// The certificate the server presents, DER ([`NO_DEV_CERTIFICATE`]): the `id-effect dev`
  /// certificate ([`is_dev_certificate`]) passes in `dev` and `test` and fails in every other
  /// profile (C13).
  pub fn tls_certificate(mut self, der: &[u8]) -> Self {
    let dev = is_dev_certificate(der);
    self.certificate = Some(match (dev, Enforcement::for_profile(&self.profile)) {
      (false, _) => {
        Stated::Pass("the TLS certificate is not an id-effect dev certificate".to_owned())
      }
      (true, Enforcement::Report) => Stated::Pass(format!(
        "the id-effect dev certificate, allowed in the {} profile only",
        self.profile
      )),
      (true, Enforcement::Refuse) => Stated::Fail(
        "the TLS certificate is the id-effect dev certificate: use certificate files or ACME"
          .to_owned(),
      ),
    });
    self
  }

  /// Whether HTTP/3 is on and whether the host has TLS ([`HTTP3_WITH_TLS`]).
  pub fn http3(mut self, enabled: bool, tls: bool) -> Self {
    self.http3 = Some(match (enabled, tls) {
      (false, _) => Stated::Absent("HTTP/3 is off (server.http3.enabled = false)".to_owned()),
      (true, true) => {
        Stated::Pass("HTTP/3 on the TCP port, with the host's certificate".to_owned())
      }
      (true, false) => Stated::Fail(
        "HTTP/3 is enabled without TLS: set certificate files (or ACME), or turn it off behind a \
         load balancer (server.http3.enabled = false)"
          .to_owned(),
      ),
    });
    self
  }

  /// Whether HTTP/3 admits 0-RTT early data ([`ZERO_RTT_OFF`]).
  pub fn zero_rtt(mut self, admitted: bool) -> Self {
    self.zero_rtt = Some(if admitted {
      Stated::Fail("HTTP/3 0-RTT early data is admitted: it can be replayed".to_owned())
    } else {
      Stated::Pass("HTTP/3 refuses 0-RTT early data".to_owned())
    });
    self
  }

  /// [`zero_rtt`](Self::zero_rtt) from the host's `Http3Options`.
  #[cfg(feature = "http3")]
  pub fn http3_options(self, options: &id_effect_axum::server::Http3Options) -> Self {
    self.zero_rtt(options.zero_rtt_for_queries())
  }

  /// Whether the QUIC endpoint validates client addresses with Retry ([`QUIC_RETRY_ON`];
  /// `QuicEndpointConfig::retry`).
  pub fn quic_retry(mut self, on: bool) -> Self {
    self.quic_retry = Some(if on {
      Stated::Pass("the QUIC endpoint validates client addresses (Retry)".to_owned())
    } else {
      Stated::Fail("the QUIC endpoint accepts unvalidated client addresses".to_owned())
    });
    self
  }

  /// The procedures (or routes) the app serves and the access each declares
  /// ([`PROCEDURES_DECLARED`]); repeated calls add to the list.
  pub fn procedures<I, S>(mut self, procedures: I) -> Self
  where
    I: IntoIterator<Item = (S, DeclaredAccess)>,
    S: Into<String>,
  {
    self.procedures.get_or_insert_with(Vec::new).extend(
      procedures
        .into_iter()
        .map(|(tag, access)| (tag.into(), access)),
    );
    self
  }

  /// [`procedures`](Self::procedures) from a typed router's `AccessReport`: its public and
  /// guarded routes, and the ones added with the deprecated `route_unchecked` (undeclared).
  #[cfg(feature = "web")]
  pub fn access_report(self, report: &id_effect_axum::route::AccessReport) -> Self {
    let public = report
      .public
      .iter()
      .map(|r| (r.clone(), DeclaredAccess::Public));
    let guarded = report
      .guarded
      .iter()
      .map(|(r, _)| (r.clone(), DeclaredAccess::Guarded));
    let unchecked = report
      .unchecked
      .iter()
      .map(|r| (r.clone(), DeclaredAccess::Undeclared));
    self.procedures(public.chain(guarded).chain(unchecked))
  }

  /// [`procedures`](Self::procedures) from an RPC group (every registered procedure).
  #[cfg(feature = "rpc")]
  pub fn rpc_group(self, group: &id_effect_rpc::RpcGroup) -> Self {
    let entries: Vec<(String, DeclaredAccess)> = group
      .entries()
      .map(|e| (format!("{}@{}", e.tag(), e.version()), declared(e.access())))
      .collect();
    self.procedures(entries)
  }

  /// [`procedures`](Self::procedures) from a service contract (`<Trait>Service::contract()`).
  #[cfg(feature = "rpc")]
  pub fn rpc_service(self, service: &id_effect_rpc::ServiceContract) -> Self {
    let entries: Vec<(String, DeclaredAccess)> = service
      .procedures
      .iter()
      .map(|p| {
        (
          format!("{}.{}@{}", service.name, p.name, service.version),
          declared(&p.access),
        )
      })
      .collect();
    self.procedures(entries)
  }

  /// An origin allowlist of a WebSocket or WebTransport endpoint (`what` names it in the
  /// report): whether it admits any origin and how many entries it lists
  /// ([`REALTIME_ORIGINS_SET`]); repeated calls add to the list.
  pub fn realtime_origins(mut self, what: impl Into<String>, any: bool, allowed: usize) -> Self {
    self
      .origins
      .get_or_insert_with(Vec::new)
      .push(OriginStatement {
        what: what.into(),
        any,
        allowed,
      });
    self
  }

  /// [`realtime_origins`](Self::realtime_origins) from an `OriginPolicy`.
  #[cfg(feature = "rpc-ws")]
  pub fn origin_policy(
    self,
    what: impl Into<String>,
    policy: &id_effect_axum::realtime::origin::OriginPolicy,
  ) -> Self {
    self.realtime_origins(what, policy.is_any(), policy.entries().len())
  }

  /// The origin allowlist of the RPC WebSocket (`RpcServer::with_websocket`).
  #[cfg(feature = "rpc-ws")]
  pub fn rpc_websocket(self, config: &id_effect_rpc::transport::ws::RpcWsConfig) -> Self {
    self.origin_policy("the RPC WebSocket", config.socket_config().origin_policy())
  }

  /// The origin allowlist of a WebTransport endpoint.
  #[cfg(feature = "webtransport")]
  pub fn webtransport(
    self,
    config: &id_effect_axum::server::webtransport::WebTransportConfig,
  ) -> Self {
    self.origin_policy("WebTransport", config.origin_policy())
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
    let procedures = self.procedures.as_deref().map(procedures_stated);
    let origins = self.origins.as_deref().map(origins_stated);
    let stated = [
      (CONSOLE_GUARDED, self.console.as_ref()),
      (DOCS_CLOSED, self.docs.as_ref()),
      (MESH_ENCRYPTED, self.mesh.as_ref()),
      (REDACTION_ON, self.redaction.as_ref()),
      (SSRF_ON, None),
      (CSRF_ON, self.csrf.as_ref()),
      (LOAD_SHEDDING_ON, self.load_shedding.as_ref()),
      (NO_MEMORY_ONLY_STATE, None),
      (PROCEDURES_DECLARED, procedures.as_ref()),
      (REALTIME_ORIGINS_SET, origins.as_ref()),
      (ZERO_RTT_OFF, self.zero_rtt.as_ref()),
      (QUIC_RETRY_ON, self.quic_retry.as_ref()),
      (HTTP3_WITH_TLS, self.http3.as_ref()),
      (NO_DEV_CERTIFICATE, self.certificate.as_ref()),
    ];
    let mut probes: Vec<Probe> = stated
      .into_iter()
      .map(|(name, stated)| self.probe(name, stated, &opt_outs))
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

#[cfg(feature = "rpc")]
fn declared(access: &id_effect_rpc::ProcedureAccess) -> DeclaredAccess {
  if access.is_public() {
    DeclaredAccess::Public
  } else {
    DeclaredAccess::Guarded
  }
}

fn procedures_stated(procedures: &[(String, DeclaredAccess)]) -> Stated {
  if procedures.is_empty() {
    return Stated::Absent("no procedure served".to_owned());
  }
  let count = |access| procedures.iter().filter(|(_, a)| *a == access).count();
  let undeclared: Vec<&str> = procedures
    .iter()
    .filter(|(_, a)| *a == DeclaredAccess::Undeclared)
    .map(|(tag, _)| tag.as_str())
    .collect();
  if undeclared.is_empty() {
    Stated::Pass(format!(
      "{} procedures, each declared: {} public, {} guarded",
      procedures.len(),
      count(DeclaredAccess::Public),
      count(DeclaredAccess::Guarded)
    ))
  } else {
    Stated::Fail(format!(
      "served without an access decision: {}",
      undeclared.join(", ")
    ))
  }
}

/// The verdict over the stated allowlists (at least one: [`Posture::realtime_origins`] adds).
fn origins_stated(origins: &[OriginStatement]) -> Stated {
  let mut failed = Vec::new();
  for origin in origins {
    if origin.any {
      failed.push(format!("{} admits every origin", origin.what));
    } else if origin.allowed == 0 {
      failed.push(format!(
        "{} lists no origin (browsers are refused; configure the allowlist)",
        origin.what
      ));
    }
  }
  if failed.is_empty() {
    let listed: Vec<String> = origins
      .iter()
      .map(|o| format!("{}: {} origins", o.what, o.allowed))
      .collect();
    Stated::Pass(format!(
      "Origin is checked against an allowlist ({})",
      listed.join(", ")
    ))
  } else {
    Stated::Fail(failed.join("; "))
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
    PROCEDURES_DECLARED => (
      ProbeStatus::NotApplicable,
      "no procedures stated".to_owned(),
    ),
    REALTIME_ORIGINS_SET => (
      ProbeStatus::NotApplicable,
      "no WebSocket or WebTransport endpoint stated".to_owned(),
    ),
    HTTP3_WITH_TLS => (ProbeStatus::NotApplicable, "no HTTP/3 stated".to_owned()),
    NO_DEV_CERTIFICATE => (
      ProbeStatus::NotApplicable,
      "no TLS certificate stated".to_owned(),
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
    DOCS_CLOSED => {
      id.starts_with("id_effect_axum::openapi::") || id.starts_with("id_effect_rpc::openapi::")
    }
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
        || id == "id_effect_platform::QuicEndpointConfig::dangerous_raise_limits"
    }
    NO_MEMORY_ONLY_STATE => id.ends_with("::memory_only"),
    PROCEDURES_DECLARED => {
      id == "id_effect_rpc::channels::dangerous_allow_all_joins"
        || id == "id_effect_axum::route::Router::route_unchecked"
        || id == "id_effect_axum::route::Router::allow_unregistered_routes_dangerous"
    }
    REALTIME_ORIGINS_SET => id.ends_with("::dangerous_any_origin"),
    ZERO_RTT_OFF => id.ends_with("::dangerous_allow_zero_rtt_for_queries"),
    QUIC_RETRY_ON => id == "id_effect_platform::QuicEndpointConfig::dangerous_no_retry",
    HTTP3_WITH_TLS | NO_DEV_CERTIFICATE => false,
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
      (
        NO_MEMORY_ONLY_STATE,
        "id_effect_auth::MemoryThrottleStore::memory_only",
      ),
      (
        DOCS_CLOSED,
        "id_effect_rpc::openapi::ContractDocs::public_dangerous",
      ),
      (
        LOAD_SHEDDING_ON,
        "id_effect_platform::QuicEndpointConfig::dangerous_raise_limits",
      ),
      (
        PROCEDURES_DECLARED,
        "id_effect_rpc::channels::dangerous_allow_all_joins",
      ),
      (
        PROCEDURES_DECLARED,
        "id_effect_axum::route::Router::route_unchecked",
      ),
      (
        REALTIME_ORIGINS_SET,
        "id_effect_axum::realtime::origin::OriginPolicy::dangerous_any_origin",
      ),
      (
        ZERO_RTT_OFF,
        "id_effect_axum::server::h3::Http3Options::dangerous_allow_zero_rtt_for_queries",
      ),
      (
        QUIC_RETRY_ON,
        "id_effect_platform::QuicEndpointConfig::dangerous_no_retry",
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
