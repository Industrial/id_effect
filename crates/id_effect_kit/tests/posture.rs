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
