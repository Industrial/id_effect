//! The startup posture check (ADR 0013 R10): probes, accepted risks, prod enforcement.
//!
//! The posture registry is process-wide, so the tests that record an opt-out use ids of their
//! own and assert only about those; the rest evaluate an explicit opt-out list.

use std::path::PathBuf;

use id_effect::observability::posture::record_dangerous;
use id_effect_kit::config::Profiles;
use id_effect_kit::posture::{
  self, AcceptedRisks, CONSOLE_GUARDED, CSRF_ON, Enforcement, LOAD_SHEDDING_ON, MESH_ENCRYPTED,
  NO_UNACCEPTED_OPT_OUT, PROBES, Posture, ProbeStatus, Surface,
};

/// A fresh directory under the system temp dir with `files` written into it.
fn config_dir(name: &str, files: &[(&str, &str)]) -> PathBuf {
  let dir = std::env::temp_dir().join(format!(
    "id_effect_kit-posture-{name}-{}",
    std::process::id()
  ));
  std::fs::create_dir_all(&dir).unwrap();
  for (file, body) in files {
    std::fs::write(dir.join(file), body).unwrap();
  }
  dir
}

#[test]
fn posture_report_lists_probes() {
  let report = posture::check("dev");
  let names: Vec<&str> = report.probes().iter().map(|p| p.name).collect();
  assert_eq!(names, PROBES);
  assert_eq!(report.profile(), "dev");
  assert_eq!(report.enforcement(), Enforcement::Report);
  let shown = report.to_string();
  for probe in report.probes() {
    assert!(!probe.detail.is_empty(), "{}", probe.name);
    assert!(shown.contains(probe.name), "{shown}");
  }
  // Nothing stated: the surfaces are not applicable, the defaults pass.
  let quiet = Posture::new("prod").evaluate(&[]);
  assert!(quiet.passed(), "{quiet}");
  for name in [CONSOLE_GUARDED, MESH_ENCRYPTED, CSRF_ON] {
    assert_eq!(
      quiet.probe(name).unwrap().status,
      ProbeStatus::NotApplicable
    );
  }
  assert_eq!(
    quiet.probe(LOAD_SHEDDING_ON).unwrap().status,
    ProbeStatus::Pass
  );
}

#[test]
fn posture_prod_fails_on_unaccepted_opt_out() {
  const ID: &str = "id_effect_kit::tests::Fixture::dangerous_prod_gate";
  record_dangerous(ID, "the prod gate fixture");

  let prod = posture::check("prod");
  let probe = prod.probe(NO_UNACCEPTED_OPT_OUT).unwrap();
  assert_eq!(probe.status, ProbeStatus::Fail);
  assert!(probe.detail.contains(ID), "{}", probe.detail);
  assert!(
    prod
      .opt_outs()
      .iter()
      .any(|o| o.id == ID && o.accepted.is_none())
  );
  let refused = prod.enforce().unwrap_err();
  assert!(
    refused.to_string().contains("refuses to start"),
    "{refused}"
  );
  assert!(refused.to_string().contains(ID), "{refused}");
  // Any profile but dev and test refuses.
  assert!(posture::check("staging").enforce().is_err());

  // dev and test report the same failure and start.
  for profile in ["dev", "test"] {
    let report = posture::check(profile);
    assert!(!report.passed());
    assert!(report.enforce().is_ok(), "{profile}");
  }

  // Accepted with a reason, the opt-out no longer fails the probe.
  let accepted = Posture::new("prod")
    .accepted_risks(AcceptedRisks::new().accept(ID, "test fixture, never in a binary"))
    .check();
  // Other tests of this binary may record opt-outs of their own; this one is no longer a
  // reason for the probe to fail.
  let probe = accepted.probe(NO_UNACCEPTED_OPT_OUT).unwrap();
  assert!(
    probe.status != ProbeStatus::Fail || !probe.detail.contains(ID),
    "{}",
    probe.detail
  );
  let fixture = accepted.opt_outs().iter().find(|o| o.id == ID).unwrap();
  assert_eq!(
    fixture.accepted.as_deref(),
    Some("test fixture, never in a binary")
  );
}

#[test]
fn posture_accepted_risk_needs_reason() {
  const ID: &str = "id_effect_kit::tests::Fixture::dangerous_needs_reason";
  let opt_outs = [(ID, "the reason fixture")];

  // Listed with a blank reason: not accepted.
  let blank = Posture::new("prod")
    .accepted_risks(AcceptedRisks::new().accept(ID, "  "))
    .evaluate(&opt_outs);
  let probe = blank.probe(NO_UNACCEPTED_OPT_OUT).unwrap();
  assert_eq!(probe.status, ProbeStatus::Fail);
  assert!(
    probe.detail.contains("without a reason"),
    "{}",
    probe.detail
  );
  assert!(blank.enforce().is_err());

  // With a reason: accepted, and the report passes.
  let reasoned = Posture::new("prod")
    .accepted_risks(AcceptedRisks::new().accept(ID, "reviewed 2026-10"))
    .evaluate(&opt_outs);
  assert_eq!(
    reasoned.probe(NO_UNACCEPTED_OPT_OUT).unwrap().status,
    ProbeStatus::Accepted
  );
  assert!(reasoned.enforce().is_ok());

  // The same from the profile's configuration (`security.accepted_risks`).
  let dir = config_dir(
    "reason",
    &[
      (
        "prod.toml",
        "[security.accepted_risks]\n\"id_effect_kit::tests::Fixture::dangerous_needs_reason\" = \"\"\n",
      ),
      (
        "test.toml",
        "[security.accepted_risks]\n\"id_effect_kit::tests::Fixture::dangerous_needs_reason\" = \"reviewed\"\n",
      ),
      ("dev.toml", "[security]\naccepted_risks = 3\n"),
    ],
  );
  let profiles = Profiles::new()
    .dir(&dir)
    .env_prefix("ID_EFFECT_KIT_POSTURE_TEST_");
  let load = |name: &str| {
    let config = profiles.load_for(profiles.profile(name).unwrap()).unwrap();
    AcceptedRisks::from_config(&config).map_err(|e| e.to_string())
  };
  let prod = load("prod").unwrap();
  assert!(prod.lists(ID));
  assert_eq!(prod.reason(ID), None);
  let blank = Posture::new("prod")
    .accepted_risks(prod)
    .evaluate(&opt_outs);
  assert!(blank.enforce().is_err());
  assert_eq!(load("test").unwrap().reason(ID), Some("reviewed"));
  assert!(load("dev").is_err(), "a non-table value is a config error");
  // No key at all: nothing accepted.
  let empty = config_dir("empty", &[("prod.toml", "port = 1\n")]);
  let profiles = Profiles::new().dir(&empty);
  let config = profiles
    .load_for(profiles.profile("prod").unwrap())
    .unwrap();
  assert_eq!(
    AcceptedRisks::from_config(&config).unwrap(),
    AcceptedRisks::new()
  );
  let _ = std::fs::remove_dir_all(dir);
  let _ = std::fs::remove_dir_all(empty);
}

#[test]
fn stated_surfaces_decide_their_probes() {
  let open = Posture::new("prod")
    .console(Surface::Open)
    .mesh_encrypted(false)
    .csrf(false)
    .max_in_flight(None)
    .evaluate(&[]);
  let failed: Vec<&str> = open.failures().map(|p| p.name).collect();
  assert_eq!(
    failed,
    [CONSOLE_GUARDED, MESH_ENCRYPTED, CSRF_ON, LOAD_SHEDDING_ON]
  );
  let closed = Posture::new("prod")
    .console(Surface::Guarded)
    .docs(Surface::Absent)
    .mesh_encrypted(true)
    .csrf(true)
    .max_in_flight(Some(64))
    .evaluate(&[]);
  assert!(closed.passed(), "{closed}");
  assert_eq!(
    closed.probe(CONSOLE_GUARDED).unwrap().status,
    ProbeStatus::Pass
  );
}

/// The ids the batteries record land on the probe of their rule.
#[cfg(all(
  feature = "web",
  feature = "cluster",
  feature = "http-client",
  feature = "auth-totp",
  feature = "auth-oidc",
  feature = "logger"
))]
#[test]
fn battery_opt_outs_fail_their_probes() {
  use id_effect_kit::posture::{NO_MEMORY_ONLY_STATE, REDACTION_ON, SSRF_ON};

  let cases = [
    (
      MESH_ENCRYPTED,
      id_effect_kit::node::transport::tcp::DANGEROUS_PLAINTEXT,
    ),
    (
      MESH_ENCRYPTED,
      id_effect_kit::node::transport::tls_tcp::DANGEROUS_NO_CLIENT_AUTH,
    ),
    (
      SSRF_ON,
      id_effect_kit::platform::http::ssrf::DANGEROUS_ALLOW_PRIVATE_NETWORKS,
    ),
    (SSRF_ON, id_effect_kit::auth::oidc::INSECURE_ALLOW_HTTP),
    (
      NO_MEMORY_ONLY_STATE,
      id_effect_kit::auth::totp::MEMORY_TOTP_STEP_STORE_POSTURE,
    ),
    (
      NO_MEMORY_ONLY_STATE,
      id_effect_kit::auth::MEMORY_THROTTLE_STORE_POSTURE,
    ),
    (
      REDACTION_ON,
      "id_effect_logger::RedactionPolicy::dangerous_none",
    ),
    (REDACTION_ON, id_effect_kit::logger::DANGEROUS_EMPTY),
    (
      CSRF_ON,
      id_effect_kit::axum::server::security::DANGEROUS_WITHOUT_CSRF,
    ),
  ];
  for (probe, id) in cases {
    let report = Posture::new("prod").evaluate(&[(id, "reopens")]);
    assert_eq!(
      report.probe(probe).unwrap().status,
      ProbeStatus::Fail,
      "{id}"
    );
  }
  let host = id_effect_kit::axum::HostConfig::default();
  let report = Posture::new("prod").host(&host).evaluate(&[]);
  assert_eq!(
    report.probe(LOAD_SHEDDING_ON).unwrap().status,
    ProbeStatus::Pass
  );
  let redaction = id_effect_kit::logger::RedactionPolicy::recommended();
  let report = Posture::new("prod").redaction(&redaction).evaluate(&[]);
  assert_eq!(
    report.probe(REDACTION_ON).unwrap().status,
    ProbeStatus::Pass
  );
}

/// The CSRF probe reads the layer's own state, not an app-stated flag.
#[cfg(feature = "web")]
#[test]
fn csrf_probe_reads_the_security_layer() {
  use id_effect_kit::axum::server::security::{CookieKey, SecurityLayer};

  let layer = SecurityLayer::recommended(CookieKey::generate());
  let report = Posture::new("prod").security_layer(&layer).evaluate(&[]);
  assert_eq!(report.probe(CSRF_ON).unwrap().status, ProbeStatus::Pass);
  let layer = layer.dangerous_without_csrf();
  let report = Posture::new("prod").security_layer(&layer).evaluate(&[]);
  assert_eq!(report.probe(CSRF_ON).unwrap().status, ProbeStatus::Fail);
}

/// A self-signed ECDSA certificate for `localhost`; `dev` gives it the organization every
/// `id-effect dev` certificate carries.
fn certificate(dev: bool) -> (rcgen::Certificate, rcgen::KeyPair) {
  let mut params = rcgen::CertificateParams::new(vec!["localhost".to_owned()]).unwrap();
  let organization = if dev {
    posture::DEV_CERTIFICATE_ORGANIZATION
  } else {
    "Example Ltd"
  };
  params
    .distinguished_name
    .push(rcgen::DnType::OrganizationName, organization);
  let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
  (params.self_signed(&key).unwrap(), key)
}

/// C13: the prod profile refuses the `id-effect dev` certificate; dev and test serve it.
#[test]
fn posture_prod_refuses_dev_certificate() {
  use id_effect_kit::posture::{NO_DEV_CERTIFICATE, is_dev_certificate};

  let (dev, _) = certificate(true);
  let (real, _) = certificate(false);
  assert!(is_dev_certificate(dev.der()));
  assert!(!is_dev_certificate(real.der()));

  let prod = Posture::new("prod")
    .tls_certificate(dev.der())
    .evaluate(&[]);
  let probe = prod.probe(NO_DEV_CERTIFICATE).unwrap();
  assert_eq!(probe.status, ProbeStatus::Fail, "{prod}");
  assert!(probe.detail.contains("dev certificate"), "{}", probe.detail);
  let refused = prod.enforce().unwrap_err();
  assert!(
    refused.to_string().contains(NO_DEV_CERTIFICATE),
    "{refused}"
  );
  // An unknown profile is production too.
  let staging = Posture::new("staging")
    .tls_certificate(dev.der())
    .evaluate(&[]);
  assert!(staging.enforce().is_err());

  for profile in ["dev", "test"] {
    let report = Posture::new(profile)
      .tls_certificate(dev.der())
      .evaluate(&[]);
    assert_eq!(
      report.probe(NO_DEV_CERTIFICATE).unwrap().status,
      ProbeStatus::Pass,
      "{report}"
    );
    assert!(report.passed(), "{report}");
  }
  let served = Posture::new("prod")
    .tls_certificate(real.der())
    .evaluate(&[]);
  assert_eq!(
    served.probe(NO_DEV_CERTIFICATE).unwrap().status,
    ProbeStatus::Pass
  );
  assert!(served.enforce().is_ok());
  // Nothing stated: not applicable.
  assert_eq!(
    Posture::new("prod")
      .evaluate(&[])
      .probe(NO_DEV_CERTIFICATE)
      .unwrap()
      .status,
    ProbeStatus::NotApplicable
  );
}

/// The host's certificate files are read for the probe (feature `http3`).
#[cfg(feature = "http3")]
#[test]
fn posture_reads_the_host_certificate() {
  use id_effect_kit::axum::HostConfig;
  use id_effect_kit::axum::server::HostTls;
  use id_effect_kit::posture::{HTTP3_WITH_TLS, NO_DEV_CERTIFICATE};

  let (dev, key) = certificate(true);
  let dir = config_dir(
    "host-cert",
    &[
      ("dev-cert.pem", dev.pem().as_str()),
      ("dev-key.pem", key.serialize_pem().as_str()),
    ],
  );
  let host = HostConfig::default().with_tls(HostTls::files(
    dir.join("dev-cert.pem"),
    dir.join("dev-key.pem"),
  ));
  let prod = Posture::new("prod").host(&host).evaluate(&[]);
  assert_eq!(
    prod.probe(NO_DEV_CERTIFICATE).unwrap().status,
    ProbeStatus::Fail,
    "{prod}"
  );
  assert_eq!(
    prod.probe(HTTP3_WITH_TLS).unwrap().status,
    ProbeStatus::Pass
  );
  let dev_profile = Posture::new("dev").host(&host).evaluate(&[]);
  assert_eq!(
    dev_profile.probe(NO_DEV_CERTIFICATE).unwrap().status,
    ProbeStatus::Pass
  );

  // A provided identity is read as well.
  let provided = id_effect_kit::axum::server::ServerTls::from_pem_files(
    dir.join("dev-cert.pem"),
    dir.join("dev-key.pem"),
    None,
  )
  .unwrap();
  let host = HostConfig::default().with_tls(HostTls::provided(provided));
  let report = Posture::new("prod").host(&host).evaluate(&[]);
  assert_eq!(
    report.probe(NO_DEV_CERTIFICATE).unwrap().status,
    ProbeStatus::Fail
  );

  // Settings without an identity (a resolver, ACME) state no certificate.
  let resolver = id_effect_kit::platform::tls::server_tls_config_with(
    std::sync::Arc::new(
      id_effect_kit::platform::tls::Identity::from_certificate(
        &id_effect_kit::platform::tls::NodeCertificate::self_signed("localhost").unwrap(),
      )
      .unwrap(),
    ),
    None,
  )
  .unwrap();
  let host = HostConfig::default().with_tls(HostTls::provided(
    id_effect_kit::axum::server::ServerTls::from_rustls_config(resolver),
  ));
  let report = Posture::new("prod").host(&host).evaluate(&[]);
  assert_eq!(
    report.probe(NO_DEV_CERTIFICATE).unwrap().status,
    ProbeStatus::NotApplicable
  );

  // Files that cannot be read fail the probe; no TLS fails HTTP/3; HTTP/3 off is n/a.
  let missing = HostConfig::default().with_tls(HostTls::files(
    dir.join("missing.pem"),
    dir.join("missing-key.pem"),
  ));
  let report = Posture::new("prod").host(&missing).evaluate(&[]);
  let probe = report.probe(NO_DEV_CERTIFICATE).unwrap();
  assert_eq!(probe.status, ProbeStatus::Fail);
  assert!(probe.detail.contains("cannot be read"), "{}", probe.detail);
  let plain = Posture::new("prod")
    .host(&HostConfig::default())
    .evaluate(&[]);
  assert_eq!(
    plain.probe(HTTP3_WITH_TLS).unwrap().status,
    ProbeStatus::Fail
  );
  assert_eq!(
    plain.probe(NO_DEV_CERTIFICATE).unwrap().status,
    ProbeStatus::NotApplicable
  );
  let off = HostConfig {
    http3: false,
    ..HostConfig::default()
  };
  let report = Posture::new("prod").host(&off).evaluate(&[]);
  assert_eq!(
    report.probe(HTTP3_WITH_TLS).unwrap().status,
    ProbeStatus::NotApplicable
  );
  let _ = std::fs::remove_dir_all(dir);
}

/// The 0.12 probes: procedures declared, realtime origins, 0-RTT, QUIC Retry, HTTP/3 with TLS.
#[test]
fn posture_reports_rpc_probes() {
  use id_effect_kit::posture::{
    DeclaredAccess, HTTP3_WITH_TLS, PROCEDURES_DECLARED, QUIC_RETRY_ON, REALTIME_ORIGINS_SET,
    ZERO_RTT_OFF,
  };

  // The defaults: nothing stated and no opt-out recorded.
  let quiet = Posture::new("prod").evaluate(&[]);
  for (name, status) in [
    (PROCEDURES_DECLARED, ProbeStatus::NotApplicable),
    (REALTIME_ORIGINS_SET, ProbeStatus::NotApplicable),
    (ZERO_RTT_OFF, ProbeStatus::Pass),
    (QUIC_RETRY_ON, ProbeStatus::Pass),
    (HTTP3_WITH_TLS, ProbeStatus::NotApplicable),
  ] {
    assert_eq!(quiet.probe(name).unwrap().status, status, "{name}");
  }

  let good = Posture::new("prod")
    .procedures([
      ("Greeter.greet@1", DeclaredAccess::Public),
      ("Greeter.admin@1", DeclaredAccess::Guarded),
    ])
    .realtime_origins("the RPC WebSocket", false, 1)
    .realtime_origins("WebTransport", false, 2)
    .zero_rtt(false)
    .quic_retry(true)
    .http3(true, true)
    .evaluate(&[]);
  assert!(good.passed(), "{good}");
  let procedures = good.probe(PROCEDURES_DECLARED).unwrap();
  assert!(
    procedures.detail.contains("1 public, 1 guarded"),
    "{}",
    procedures.detail
  );
  assert!(
    good
      .probe(REALTIME_ORIGINS_SET)
      .unwrap()
      .detail
      .contains("WebTransport: 2 origins")
  );

  let bad = Posture::new("prod")
    .realtime_origins("the RPC WebSocket", true, 0)
    .realtime_origins("WebTransport", false, 0)
    .zero_rtt(true)
    .quic_retry(false)
    .http3(true, false)
    .evaluate(&[]);
  let failed: Vec<&str> = bad.failures().map(|p| p.name).collect();
  assert_eq!(
    failed,
    [
      REALTIME_ORIGINS_SET,
      ZERO_RTT_OFF,
      QUIC_RETRY_ON,
      HTTP3_WITH_TLS
    ]
  );
  let origins = &bad.probe(REALTIME_ORIGINS_SET).unwrap().detail;
  assert!(origins.contains("admits every origin"), "{origins}");
  assert!(origins.contains("lists no origin"), "{origins}");
  assert!(bad.enforce().is_err());
  let off = Posture::new("prod").http3(false, false).evaluate(&[]);
  assert_eq!(
    off.probe(HTTP3_WITH_TLS).unwrap().status,
    ProbeStatus::NotApplicable
  );
  let none = Posture::new("prod")
    .procedures(Vec::<(String, DeclaredAccess)>::new())
    .evaluate(&[]);
  assert_eq!(
    none.probe(PROCEDURES_DECLARED).unwrap().status,
    ProbeStatus::NotApplicable
  );

  // The 0.12 opt-outs land on their probes.
  for (probe, id) in [
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
    (
      LOAD_SHEDDING_ON,
      "id_effect_platform::QuicEndpointConfig::dangerous_raise_limits",
    ),
    (
      PROCEDURES_DECLARED,
      "id_effect_rpc::channels::dangerous_allow_all_joins",
    ),
    (
      posture::DOCS_CLOSED,
      "id_effect_rpc::openapi::ContractDocs::public_dangerous",
    ),
  ] {
    let report = Posture::new("prod").evaluate(&[(id, "reopens")]);
    assert_eq!(
      report.probe(probe).unwrap().status,
      ProbeStatus::Fail,
      "{id}"
    );
    let accepted = Posture::new("prod")
      .accepted_risks(AcceptedRisks::new().accept(id, "reviewed"))
      .evaluate(&[(id, "reopens")]);
    assert_eq!(
      accepted.probe(probe).unwrap().status,
      ProbeStatus::Accepted,
      "{id}"
    );
  }
}

/// The probes read the batteries' own types (features `rpc`, `rpc-ws`, `http3`, `webtransport`).
#[cfg(all(feature = "rpc-ws", feature = "webtransport"))]
#[test]
fn rpc_probes_read_the_batteries() {
  use std::sync::Arc;

  use id_effect_kit::axum::realtime::origin::{DEV_ORIGINS, OriginPolicy};
  use id_effect_kit::axum::server::Http3Options;
  use id_effect_kit::axum::server::webtransport::WebTransportConfig;
  use id_effect_kit::id_effect::{schema, succeed};
  use id_effect_kit::posture::{PROCEDURES_DECLARED, REALTIME_ORIGINS_SET, ZERO_RTT_OFF};
  use id_effect_kit::rpc::transport::ws::RpcWsConfig;
  use id_effect_kit::rpc::{Procedure, RpcGroup};

  /// Refuses everyone.
  struct Nobody;
  impl id_effect_kit::axum::route::RouteGuard for Nobody {
    fn policy(&self) -> &str {
      "nobody"
    }
    fn check<'a>(
      &'a self,
      _: &'a axum::http::request::Parts,
    ) -> id_effect_kit::axum::route::GuardFuture<'a> {
      Box::pin(async { Err(axum::http::StatusCode::FORBIDDEN.into_response()) })
    }
  }
  use axum::response::IntoResponse;

  let mut group = RpcGroup::new();
  group.register(
    Procedure::query("Greeter.greet").public(),
    Arc::new(schema::string::<()>()),
    Arc::new(schema::string::<()>()),
    &["message"],
    |name, _env| succeed(format!("hello, {name}")),
  );
  group.register(
    Procedure::query("Greeter.admin").guard(Nobody),
    Arc::new(schema::string::<()>()),
    Arc::new(schema::string::<()>()),
    &["message"],
    |name, _env| succeed(format!("hello, {name}")),
  );
  let contract = group.service_contract("Greeter", 1);
  let report = Posture::new("prod")
    .rpc_group(&group)
    .rpc_service(&contract)
    .rpc_websocket(&RpcWsConfig::new())
    .webtransport(&WebTransportConfig::new())
    .http3_options(&Http3Options::default())
    .evaluate(&[]);
  let procedures = report.probe(PROCEDURES_DECLARED).unwrap();
  assert_eq!(procedures.status, ProbeStatus::Pass);
  assert!(
    procedures
      .detail
      .contains("4 procedures, each declared: 2 public, 2 guarded"),
    "{}",
    procedures.detail
  );
  // Default allowlists are empty: browsers are refused, the probe asks for a list.
  assert_eq!(
    report.probe(REALTIME_ORIGINS_SET).unwrap().status,
    ProbeStatus::Fail
  );
  assert_eq!(
    report.probe(ZERO_RTT_OFF).unwrap().status,
    ProbeStatus::Pass
  );
  let dev = OriginPolicy::allow(DEV_ORIGINS).unwrap();
  let report = Posture::new("dev")
    .origin_policy("the RPC WebSocket", &dev)
    .evaluate(&[]);
  assert_eq!(
    report.probe(REALTIME_ORIGINS_SET).unwrap().status,
    ProbeStatus::Pass
  );
}

/// C1 at startup: a procedure or route without an access decision fails the prod posture.
#[test]
fn posture_fails_on_undeclared_procedure() {
  use id_effect_kit::posture::{DeclaredAccess, PROCEDURES_DECLARED};

  let report = Posture::new("prod")
    .procedures([
      ("Greeter.greet@1", DeclaredAccess::Public),
      ("Legacy.list@1", DeclaredAccess::Undeclared),
    ])
    .evaluate(&[]);
  let probe = report.probe(PROCEDURES_DECLARED).unwrap();
  assert_eq!(probe.status, ProbeStatus::Fail);
  assert!(probe.detail.contains("Legacy.list@1"), "{}", probe.detail);
  assert!(!probe.detail.contains("Greeter.greet"), "{}", probe.detail);
  assert!(report.enforce().is_err());
  // dev reports and starts.
  let dev = Posture::new("dev")
    .procedures([("Legacy.list@1", DeclaredAccess::Undeclared)])
    .evaluate(&[]);
  assert!(!dev.passed() && dev.enforce().is_ok());
}

/// A typed route added with the deprecated `route_unchecked` is undeclared.
#[cfg(feature = "web")]
#[test]
fn undeclared_route_fails_the_procedures_probe() {
  use id_effect_kit::axum::route::{Input, Route, Router};
  use id_effect_kit::id_effect::{Effect, succeed};
  use id_effect_kit::posture::PROCEDURES_DECLARED;

  fn ok(_: &mut (), _: Input) -> Effect<String, std::convert::Infallible, ()> {
    succeed("ok".to_owned())
  }
  #[allow(deprecated)]
  let router: Router<()> = Router::new()
    .route(Route::get("/open").public().handler(ok))
    .route_unchecked(Route::get("/legacy").handler(ok));
  let report = Posture::new("prod")
    .access_report(&router.access_report())
    .evaluate(&[]);
  let probe = report.probe(PROCEDURES_DECLARED).unwrap();
  assert_eq!(probe.status, ProbeStatus::Fail);
  assert!(probe.detail.contains("/legacy"), "{}", probe.detail);
}
