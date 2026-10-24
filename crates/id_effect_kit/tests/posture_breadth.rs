//! The posture probes of the 0.13 breadth batteries (ADR 0013 R7, R10; ADR 0015): "admin
//! guarded", "mcp guarded", "prompts not logged" and "tenant not from header". Each fails the prod
//! profile on a violation unless its opt-out is an accepted risk.
//!
//! The posture registry is process-wide, so these tests evaluate explicit opt-out lists.

#![cfg(all(feature = "admin", feature = "mcp", feature = "tenancy"))]

use std::sync::Arc;

use id_effect_kit::admin::{AdminDatabase, AdminSite};
use id_effect_kit::ai::AiConfig;
use id_effect_kit::ai::mcp::{McpEndpoint, McpServer};
use id_effect_kit::authz::{Guard, require_role};
use id_effect_kit::config::Profiles;
use id_effect_kit::posture::{
  ADMIN_GUARDED, AcceptedRisks, CSRF_ON, DANGEROUS_LOG_PROMPTS, DANGEROUS_MCP_WRITE_TOOLS,
  DANGEROUS_TENANT_FROM_HEADER, Enforcement, MCP_GUARDED, MESH_ENCRYPTED, NO_UNACCEPTED_OPT_OUT,
  PROBES, PROMPTS_NOT_LOGGED, Posture, PostureReport, ProbeStatus, REDACTION_ON, Surface,
  TENANT_NOT_FROM_HEADER,
};
use id_effect_kit::sql::{Dialect, TestSqlClient};
use id_effect_kit::tenancy::TenantResolver;

const BREADTH: [&str; 4] = [
  ADMIN_GUARDED,
  MCP_GUARDED,
  PROMPTS_NOT_LOGGED,
  TENANT_NOT_FROM_HEADER,
];

fn status(report: &PostureReport, probe: &str) -> ProbeStatus {
  report.probe(probe).unwrap().status
}

fn detail(report: &PostureReport, probe: &str) -> String {
  report.probe(probe).unwrap().detail.clone()
}

fn guarded_site() -> AdminSite<id_effect_kit::admin::Guarded> {
  AdminSite::new().guard(Guard::new("staff", require_role("staff")))
}

fn guarded_mcp() -> McpEndpoint<id_effect_kit::ai::mcp::Guarded> {
  McpEndpoint::new(McpServer::new("docs", "1.0.0")).guard(Guard::new("mcp", require_role("agent")))
}

#[test]
fn posture_reports_breadth_probes() {
  // Every breadth probe is reported, before "no unaccepted opt-out".
  let report = Posture::new("prod").evaluate(&[]);
  let names: Vec<&str> = report.probes().iter().map(|p| p.name).collect();
  assert_eq!(names, PROBES);
  let at = |name| names.iter().position(|n| *n == name).unwrap();
  for name in BREADTH {
    assert!(at(name) < at(NO_UNACCEPTED_OPT_OUT), "{name}");
    assert!(report.to_string().contains(name), "{report}");
  }

  // Nothing stated, nothing recorded: no admin and no MCP endpoint (n/a); prompts and the
  // tenant source keep their secure defaults (pass).
  assert!(report.passed(), "{report}");
  assert_eq!(status(&report, ADMIN_GUARDED), ProbeStatus::NotApplicable);
  assert_eq!(status(&report, MCP_GUARDED), ProbeStatus::NotApplicable);
  assert_eq!(status(&report, PROMPTS_NOT_LOGGED), ProbeStatus::Pass);
  assert_eq!(status(&report, TENANT_NOT_FROM_HEADER), ProbeStatus::Pass);

  // What a secure app states: all four pass, read from the batteries themselves.
  let secure = Posture::new("prod")
    .admin_site(&guarded_site())
    .mcp_endpoint(&guarded_mcp())
    .ai_config(&AiConfig::default())
    .tenant_resolver(&TenantResolver::new())
    .evaluate(&[]);
  assert!(secure.passed(), "{secure}");
  for name in BREADTH {
    assert_eq!(status(&secure, name), ProbeStatus::Pass, "{name}: {secure}");
  }
  assert!(detail(&secure, ADMIN_GUARDED).contains("staff"), "{secure}");
  assert!(detail(&secure, MCP_GUARDED).contains("/mcp"), "{secure}");

  // The ids the probes look for are the ones the batteries record.
  assert_eq!(
    DANGEROUS_LOG_PROMPTS,
    id_effect_kit::ai::DANGEROUS_LOG_PROMPTS
  );
  assert_eq!(
    DANGEROUS_MCP_WRITE_TOOLS,
    id_effect_kit::ai::mcp::DANGEROUS_MCP_WRITE_TOOLS
  );

  // Logged prompts and a tenant header: each fails prod until its opt-out is accepted, and
  // then reads "accepted".
  let risky = Posture::new("prod")
    .prompts_logged(true)
    .tenant_header(Some("x-tenant"));
  let opt_outs = [
    (DANGEROUS_LOG_PROMPTS, "R3: prompts recorded"),
    (DANGEROUS_TENANT_FROM_HEADER, "M19: tenant from a header"),
  ];
  let refused = risky.evaluate(&opt_outs);
  for name in [PROMPTS_NOT_LOGGED, TENANT_NOT_FROM_HEADER] {
    assert_eq!(
      status(&refused, name),
      ProbeStatus::Fail,
      "{name}: {refused}"
    );
  }
  assert!(detail(&refused, TENANT_NOT_FROM_HEADER).contains("x-tenant"));
  assert!(detail(&refused, PROMPTS_NOT_LOGGED).contains(DANGEROUS_LOG_PROMPTS));
  assert!(refused.clone().enforce().is_err());
  // Stated without the registry (evaluate with no opt-out): still refused, by the statement.
  let stated_only = risky.evaluate(&[]);
  assert_eq!(status(&stated_only, PROMPTS_NOT_LOGGED), ProbeStatus::Fail);
  assert_eq!(
    status(&stated_only, TENANT_NOT_FROM_HEADER),
    ProbeStatus::Fail
  );

  let accepted = risky
    .clone()
    .accepted_risks(
      AcceptedRisks::new()
        .accept(
          DANGEROUS_LOG_PROMPTS,
          "staging traces stay in the VPC; reviewed 2026-10",
        )
        .accept(
          DANGEROUS_TENANT_FROM_HEADER,
          "the gateway sets and strips x-tenant",
        ),
    )
    .evaluate(&opt_outs);
  assert!(accepted.passed(), "{accepted}");
  for name in [PROMPTS_NOT_LOGGED, TENANT_NOT_FROM_HEADER] {
    assert_eq!(status(&accepted, name), ProbeStatus::Accepted, "{name}");
  }

  // Accepted for the stated opt-out, but another opt-out of the probe is not: still a failure.
  let partly = risky
    .accepted_risks(AcceptedRisks::new().accept(DANGEROUS_LOG_PROMPTS, "reviewed"))
    .evaluate(&[(DANGEROUS_LOG_PROMPTS, "R3")]);
  assert_eq!(status(&partly, PROMPTS_NOT_LOGGED), ProbeStatus::Accepted);
  assert_eq!(status(&partly, TENANT_NOT_FROM_HEADER), ProbeStatus::Fail);

  // The registry alone (no statement) is enough: an opt-out taken anywhere in the process fails
  // its probe.
  let registry_only = Posture::new("prod").evaluate(&opt_outs);
  assert_eq!(
    status(&registry_only, PROMPTS_NOT_LOGGED),
    ProbeStatus::Fail
  );
  assert_eq!(
    status(&registry_only, TENANT_NOT_FROM_HEADER),
    ProbeStatus::Fail
  );

  // dev and test report and start.
  let dev = Posture::new("dev").prompts_logged(true).evaluate(&[]);
  assert_eq!(dev.enforcement(), Enforcement::Report);
  assert!(!dev.passed() && dev.enforce().is_ok());
}

#[test]
fn posture_statements_without_a_surface_and_from_the_profile() {
  // Surfaces the app does not have: not applicable.
  let none = Posture::new("prod").no_mesh().no_browser().evaluate(&[]);
  assert_eq!(none.profile(), "prod");
  for name in [MESH_ENCRYPTED, CSRF_ON] {
    assert_eq!(status(&none, name), ProbeStatus::NotApplicable, "{name}");
  }

  // A stated failure and an unaccepted opt-out of the same probe: both named.
  const NONE: &str = "id_effect_logger::RedactionPolicy::dangerous_none";
  let empty = Posture::new("prod").redaction_non_empty(false);
  assert_eq!(empty.profile(), "prod");
  let report = empty.evaluate(&[(NONE, "M3: nothing redacted")]);
  assert_eq!(status(&report, REDACTION_ON), ProbeStatus::Fail);
  let why = detail(&report, REDACTION_ON);
  assert!(why.contains("empty") && why.contains(NONE), "{why}");

  // An accepted risk shows as "accepted" in the startup log, with its reason.
  let accepted = Posture::new("prod")
    .accepted_risks(AcceptedRisks::new().accept(NONE, "logs stay on the host"))
    .evaluate(&[(NONE, "M3: nothing redacted")]);
  let shown = accepted.to_string();
  assert!(
    shown.contains("accepted ") && shown.contains("logs stay on the host"),
    "{shown}"
  );

  // The accepted risks of the selected profile (`dev` unless the environment says otherwise).
  let dir = std::env::temp_dir().join(format!(
    "id_effect_kit-posture-breadth-{}",
    std::process::id()
  ));
  std::fs::create_dir_all(&dir).unwrap();
  std::fs::write(
    dir.join("dev.toml"),
    format!("[security.accepted_risks]\n\"{DANGEROUS_LOG_PROMPTS}\" = \"local traces only\"\n"),
  )
  .unwrap();
  let profiles = Profiles::new()
    .dir(&dir)
    .env_prefix("ID_EFFECT_KIT_POSTURE_BREADTH_TEST_")
    .env_var("ID_EFFECT_KIT_POSTURE_BREADTH_PROFILE")
    .default_profile("dev");
  let loaded = Posture::load(&profiles).expect("the dev profile loads");
  assert_eq!(loaded.profile(), "dev");
  let report = loaded
    .prompts_logged(true)
    .evaluate(&[(DANGEROUS_LOG_PROMPTS, "R3")]);
  assert_eq!(
    status(&report, PROMPTS_NOT_LOGGED),
    ProbeStatus::Accepted,
    "{report}"
  );
  let risks = AcceptedRisks::new().accept("b", "two").accept("a", "one");
  let listed: Vec<(&str, &str)> = risks.iter().collect();
  assert_eq!(listed, [("a", "one"), ("b", "two")]);
  std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn posture_prod_refuses_public_admin() {
  // The admin's own routes: every one behind the site's guard.
  let db = AdminDatabase::new(Arc::new(TestSqlClient::new()), Dialect::Postgres);
  let mut report = guarded_site().router::<()>(db).access_report();
  let guarded = Posture::new("prod")
    .admin_routes("/admin/", &report)
    .evaluate(&[]);
  assert_eq!(
    status(&guarded, ADMIN_GUARDED),
    ProbeStatus::Pass,
    "{guarded}"
  );

  // A hand-written public route under the admin prefix: refused in prod.
  report.public.push("GET /admin/export".to_owned());
  report.public.push("GET /administrators".to_owned());
  let public = Posture::new("prod")
    .admin_routes("/admin", &report)
    .evaluate(&[]);
  assert_eq!(status(&public, ADMIN_GUARDED), ProbeStatus::Fail);
  let why = detail(&public, ADMIN_GUARDED);
  assert!(why.contains("GET /admin/export"), "{why}");
  assert!(
    !why.contains("administrators"),
    "only routes under the prefix: {why}"
  );
  let refused = public.enforce().unwrap_err();
  assert!(refused.to_string().contains(ADMIN_GUARDED), "{refused}");

  // An undeclared (route_unchecked) admin route is no better.
  let mut unchecked = report.clone();
  unchecked.public.clear();
  unchecked.unchecked.push("POST /admin".to_owned());
  let refused = Posture::new("prod")
    .admin_routes("/admin", &unchecked)
    .evaluate(&[]);
  assert_eq!(status(&refused, ADMIN_GUARDED), ProbeStatus::Fail);

  // Stated open: refused; there is no opt-out that makes a public admin acceptable.
  let open = Posture::new("prod")
    .admin(Surface::Open)
    .accepted_risks(AcceptedRisks::new().accept("id_effect_admin::anything", "no"))
    .evaluate(&[]);
  assert_eq!(status(&open, ADMIN_GUARDED), ProbeStatus::Fail);
  assert!(open.enforce().is_err());

  // No admin route at all, or an admin stated absent: not applicable.
  let none = Posture::new("prod")
    .admin_routes(
      "/admin",
      &id_effect_kit::axum::route::AccessReport::default(),
    )
    .evaluate(&[]);
  assert_eq!(status(&none, ADMIN_GUARDED), ProbeStatus::NotApplicable);
  let absent = Posture::new("prod").admin(Surface::Absent).evaluate(&[]);
  assert_eq!(status(&absent, ADMIN_GUARDED), ProbeStatus::NotApplicable);
  let stated = Posture::new("prod").admin(Surface::Guarded).evaluate(&[]);
  assert_eq!(status(&stated, ADMIN_GUARDED), ProbeStatus::Pass);

  // dev reports a public admin and starts.
  let dev = Posture::new("dev").admin(Surface::Open).evaluate(&[]);
  assert!(dev.enforce().is_ok());
}

#[test]
fn posture_prod_refuses_unguarded_mcp() {
  // The endpoint's own route: guarded.
  let mut report = guarded_mcp().router::<()>().access_report();
  let guarded = Posture::new("prod")
    .mcp_routes("/mcp", &report)
    .evaluate(&[]);
  assert_eq!(
    status(&guarded, MCP_GUARDED),
    ProbeStatus::Pass,
    "{guarded}"
  );

  // A public route serving MCP (a hand-mounted handler): refused in prod.
  report.public.push("POST /mcp/public".to_owned());
  let public = Posture::new("prod")
    .mcp_routes("/mcp", &report)
    .evaluate(&[]);
  assert_eq!(status(&public, MCP_GUARDED), ProbeStatus::Fail);
  assert!(public.enforce().is_err());

  // Stated open: refused.
  let open = Posture::new("prod").mcp(Surface::Open).evaluate(&[]);
  assert_eq!(status(&open, MCP_GUARDED), ProbeStatus::Fail);
  assert!(detail(&open, MCP_GUARDED).contains("anyone"), "{open}");

  // Write tools exposed over MCP (`dangerous_mcp_write_tools`): refused until accepted.
  let writes = [(DANGEROUS_MCP_WRITE_TOOLS, "R10: write tools over MCP")];
  let endpoint = Posture::new("prod").mcp_endpoint(&guarded_mcp());
  let refused = endpoint.evaluate(&writes);
  assert_eq!(status(&refused, MCP_GUARDED), ProbeStatus::Fail);
  assert!(detail(&refused, MCP_GUARDED).contains(DANGEROUS_MCP_WRITE_TOOLS));
  assert!(refused.enforce().is_err());
  let accepted = endpoint
    .accepted_risks(
      AcceptedRisks::new().accept(DANGEROUS_MCP_WRITE_TOOLS, "only staff agents; reviewed"),
    )
    .evaluate(&writes);
  assert_eq!(status(&accepted, MCP_GUARDED), ProbeStatus::Accepted);
  assert!(accepted.passed(), "{accepted}");

  // The opt-out is recorded by the server itself, so `check()` sees it too.
  let _server = McpServer::new("tools", "1.0.0").dangerous_mcp_write_tools();
  let checked = Posture::new("prod").mcp_endpoint(&guarded_mcp()).check();
  assert_eq!(
    status(&checked, MCP_GUARDED),
    ProbeStatus::Fail,
    "{checked}"
  );
}
