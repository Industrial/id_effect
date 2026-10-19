//! Every route is public or guarded (ADR 0013 R1, finding H8): the access report of the whole
//! Ledger — typed routes and the plain ones merged next to them — and the deny-by-default layer
//! that answers `404` for a route without a declaration.

mod common;

use std::sync::Arc;

use common::{As, run, with_ledger};
use id_effect_kit::test::http::{StatusCode, TestClient};
use ledger::app::declared_only;
use ledger::{Database, LedgerConfig, Services, assemble};
use serde_json::{Value, json};

/// The access report of the Ledger assembled for `config` over `db`.
fn access_report(
  config: &LedgerConfig,
  l: &common::Ledger,
) -> id_effect_kit::axum::route::AccessReport {
  let services = Services::new(Database::new(Arc::clone(&l.db), vec![]));
  assemble(config, services)
    .expect("the app assembles")
    .access_report()
}

#[tokio::test]
async fn ledger_access_report_snapshot() {
  with_ledger(
    |_, _| {},
    |l| async move {
      // The test profile: the docs are served, the console is mounted.
      let report = access_report(&l.config, &l);
      assert!(!report.has_opt_outs(), "{report}");
      assert!(report.unchecked.is_empty() && !report.unregistered_allowed);
      let public = [
        "POST /orgs",
        "POST /auth/login",
        "GET /health",
        "GET /ready",
        "POST /auth/register",
        "POST /auth/verify",
        "POST /auth/verify/resend",
        "POST /auth/password/forgot",
        "POST /auth/password/reset",
        "GET /auth/csrf",
        "GET /openapi.json",
        "GET /docs",
      ];
      assert_eq!(report.public, public, "{report}");
      let guarded = |route: &str| {
        report
          .guarded
          .iter()
          .find(|(r, _)| r == route)
          .map(|(_, policies)| policies.clone())
      };
      for (route, policy) in [
        ("POST /accounts/{id}/closure", "accounts.open"),
        ("POST /accounts/{id}/closure/approval", "accounts.close"),
        ("GET /accounts/{id}/closure", "ledger.read"),
        ("GET /_routes", "routes.read"),
        ("GET /_console", "console.admin"),
        ("PUT /_console/api/log-level", "console.admin"),
        ("POST /_console/api/jobs/{id}/retry", "console.admin"),
      ] {
        assert_eq!(guarded(route), Some(vec![policy.to_owned()]), "{report}");
      }
      for (route, policies) in &report.guarded {
        assert_eq!(policies.len(), 1, "one guard per route: {route}");
      }

      // The prod settings: no docs, no console; the rest unchanged.
      let mut prod = l.config.clone();
      prod.profile = "prod".to_owned();
      prod.console.enabled = false;
      let report = access_report(&prod, &l);
      assert_eq!(report.public, &public[..10], "{report}");
      assert!(
        report
          .guarded
          .iter()
          .all(|(route, _)| !route.contains("/_console")),
        "{report}"
      );
    },
  )
  .await;
}

#[tokio::test]
async fn h8_ledger_unregistered_route_denied() {
  with_ledger(
    |_, _| {},
    |l| async move {
      // The Ledger's plain routes answer only the methods they declare: the access layer
      // answers before the route's own `405`.
      let denied = l.post("/health", json!({}), &As::Anonymous).await;
      denied.assert_status(StatusCode::NOT_FOUND);
      let problem: Value = denied.json().unwrap();
      assert_eq!(
        problem["detail"], "no route declares access to this resource",
        "{problem}"
      );
      l.delete("/auth/csrf", &As::Anonymous)
        .await
        .assert_status(StatusCode::NOT_FOUND);
      l.get("/ready", &As::Anonymous)
        .await
        .assert_status(StatusCode::OK);

      // A plain route merged into the Ledger without a declaration is not served.
      let services = Services::new(Database::new(Arc::clone(&l.db), vec![]));
      let assembled = assemble(&l.config, services).expect("the app assembles");
      let debug = axum::Router::new().route("/debug", axum::routing::get(|| async { "internals" }));
      let app = declared_only(assembled.router.merge(debug), &assembled.routes);
      let client = TestClient::new(common::behind_proxy(app));
      run(client.get("/debug").send())
        .await
        .assert_status(StatusCode::NOT_FOUND);
      run(client.get("/health").send())
        .await
        .assert_status(StatusCode::OK);
    },
  )
  .await;
}
