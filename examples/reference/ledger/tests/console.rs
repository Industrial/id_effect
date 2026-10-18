//! The operator console (obs.console #1 #2 #3 #6): `/_console` and its API behind
//! `Guard::named(&policies, "console.admin")` — an operator who passed the TOTP step sees the
//! fibers, the supervisors, the metrics (the database series included), the health checks and the
//! job queues, and changes the log level at runtime; nobody else learns anything.

mod common;

use common::{As, totp_code, with_ledger};
use id_effect_kit::auth::TotpSecret;
use id_effect_kit::jobs::{JobOptions, JobQueueExt};
use id_effect_kit::test::http::StatusCode;
use ledger::jobs::{self, StatementsDue};
use serde_json::{Value, json};

const API: [&str; 6] = [
  "/_console/api/fibers",
  "/_console/api/supervisors",
  "/_console/api/metrics",
  "/_console/api/health",
  "/_console/api/log-level",
  "/_console/api/jobs",
];

#[tokio::test]
async fn only_an_operator_with_a_second_factor_opens_the_console() {
  with_ledger(
    |_, _| {},
    |l| async move {
      let team = l.team("acme").await;
      l.org("ops").await;
      let secret = TotpSecret::generate().expect("a secret");
      l.user("ops", "ops-admin", "operator", Some(&secret)).await;
      l.user("ops", "ops-intern", "operator", None).await;
      let operator = l.login("ops-admin", Some(totp_code(&secret, 0))).await;
      let without_mfa = l.login("ops-intern", None).await;

      // A job in the queue, for the jobs panel.
      let queue = jobs::queue(&l.db);
      common::run(queue.enqueue(
        StatementsDue {
          month: "2026-09".into(),
        },
        JobOptions::new(),
      ))
      .await;

      for path in API
        .iter()
        .copied()
        .chain(["/_console", "/_console/api/nothing"])
      {
        l.get(path, &As::Anonymous)
          .await
          .assert_status(StatusCode::UNAUTHORIZED);
        for (who, name) in [
          (&team.owner, "an owner"),
          (&team.viewer, "a viewer"),
          (&without_mfa, "an operator without TOTP"),
        ] {
          let denied = l.get(path, who).await;
          assert_eq!(denied.status(), StatusCode::FORBIDDEN, "{name} on {path}");
          assert!(!denied.text().contains("fibers"), "{name} learns nothing");
        }
      }
      for path in API {
        let res = l.get(path, &operator).await;
        res.assert_status(StatusCode::OK);
        assert_eq!(res.header("cache-control"), Some("no-store"), "{path}");
      }
      let page = l.get("/_console", &operator).await;
      page.assert_status(StatusCode::OK);
      assert!(
        page
          .header("content-type")
          .unwrap()
          .starts_with("text/html")
      );

      let fibers: Value = l
        .get("/_console/api/fibers", &operator)
        .await
        .json()
        .unwrap();
      assert!(fibers.as_array().is_some_and(|f| !f.is_empty()), "{fibers}");
      let metrics = l.get("/_console/api/metrics", &operator).await.text();
      assert!(metrics.contains("db_client_operations_total"), "{metrics}");
      let queues = l.get("/_console/api/jobs", &operator).await.text();
      assert!(queues.contains("statements"), "{queues}");

      // The log level, changed at runtime (a session needs the CSRF header for it).
      let changed = l
        .put(
          "/_console/api/log-level",
          json!({"level": "debug"}),
          &operator,
        )
        .await;
      changed.assert_status(StatusCode::OK);
      let level: Value = l
        .get("/_console/api/log-level", &operator)
        .await
        .json()
        .unwrap();
      assert_eq!(level["logger"], "debug");
      l.put(
        "/_console/api/log-level",
        json!({"level": "debug"}),
        &team.owner,
      )
      .await
      .assert_status(StatusCode::FORBIDDEN);
    },
  )
  .await;
}

#[tokio::test]
async fn the_console_is_not_mounted_when_disabled() {
  with_ledger(
    |config, _| config.console.enabled = false,
    |l| async move {
      l.get("/_console/api/fibers", &As::Anonymous)
        .await
        .assert_status(StatusCode::NOT_FOUND);
    },
  )
  .await;
}
