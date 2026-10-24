//! The Pipeline's prod posture (release spec 0.13.0, ADR 0013 R10; r13-w7-pipeline-backoffice):
//! the whole app — API, RPC, MCP, admin, review queue — assembled as `main` assembles it, then
//! the kit's startup check in the `prod` profile: every probe passes or does not apply, nothing
//! is an accepted risk, and no `dangerous_*` opt-out was taken. No database is needed (nothing
//! connects), so this test runs without Docker.

use std::sync::Arc;

use id_effect_kit::ai::AiConfig;
use id_effect_kit::ai::testing::ScriptedModel;
use id_effect_kit::posture::{
  ADMIN_GUARDED, CSRF_ON, DANGEROUS_LOG_PROMPTS, DANGEROUS_TENANT_FROM_HEADER, MCP_GUARDED,
  NO_UNACCEPTED_OPT_OUT, PROCEDURES_DECLARED, PROMPTS_NOT_LOGGED, ProbeStatus,
  TENANT_NOT_FROM_HEADER,
};
use id_effect_kit::sql::TestSqlClient;
use pipeline::App;
use pipeline::mcp::McpSettings;
use pipeline::tenancy::{TenantConfig, Tenants, TokenKeys};

/// The app over two tenants (globex on a database of its own); pools are lazy, so nothing
/// connects.
fn app() -> Arc<App> {
  let tenants = Tenants::new([
    TenantConfig::new("acme", "en").expect("acme"),
    TenantConfig::new("globex", "de")
      .expect("globex")
      .with_database("postgres://pipeline@127.0.0.1:1/globex"),
  ]);
  let router = tenants.router("postgres://pipeline@127.0.0.1:1/pipeline", 2);
  Arc::new(App::new(
    tenants,
    router,
    Arc::new(TestSqlClient::new()),
    Arc::new(ScriptedModel::new()),
    Arc::new(ScriptedModel::new()),
  ))
}

#[test]
fn pipeline_prod_posture_is_clean() {
  let app = app();
  let keys = TokenKeys::new(vec![7; 32], "https://id.pipeline.test").expect("keys");
  let mcp = McpSettings::default();
  // Assemble everything the service serves first, so every opt-out it takes is recorded.
  let _service = pipeline::router(&app, &keys, &mcp).expect("the app builds");
  let ai = AiConfig::default();
  let posture = pipeline::ui::posture(&app, &mcp, &ai, "prod").expect("the posture builds");
  let report = posture.check();

  assert!(report.passed(), "{report}");
  assert!(report.opt_outs().is_empty(), "{:?}", report.opt_outs());
  for probe in report.probes() {
    assert!(
      matches!(probe.status, ProbeStatus::Pass | ProbeStatus::NotApplicable),
      "{}: {:?} ({})",
      probe.name,
      probe.status,
      probe.detail
    );
  }
  for name in [
    ADMIN_GUARDED,
    MCP_GUARDED,
    PROMPTS_NOT_LOGGED,
    TENANT_NOT_FROM_HEADER,
    CSRF_ON,
    PROCEDURES_DECLARED,
    NO_UNACCEPTED_OPT_OUT,
  ] {
    let probe = report.probe(name).expect("the probe ran");
    assert_eq!(probe.status, ProbeStatus::Pass, "{name}: {}", probe.detail);
  }
  let admin = report.probe(ADMIN_GUARDED).expect("admin");
  assert!(admin.detail.contains("admin"), "{}", admin.detail);
  assert!(report.enforce().is_ok());

  // Zero accepted risks: the opt-outs the probes watch would each refuse the prod start.
  for opt_out in [DANGEROUS_LOG_PROMPTS, DANGEROUS_TENANT_FROM_HEADER] {
    let weakened = posture.evaluate(&[(opt_out, "taken in a test")]);
    assert!(!weakened.passed(), "{opt_out}: {weakened}");
    assert!(weakened.enforce().is_err(), "{opt_out}");
  }
}
