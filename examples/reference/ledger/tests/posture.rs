//! The production posture (ADR 0013 R7 R10; findings M6, M7): the Ledger as `main.rs` assembles
//! it under the `prod` profile — its secrets from Vault, PostgreSQL and Redis in containers, the
//! background tasks built — passes the startup posture check with no accepted risk, and serves
//! neither the console nor `/docs`.
//!
//! A test binary of its own: the posture registry is process-wide, and the other binaries take
//! test-only shortcuts that record themselves there (`common::totp_code` builds a
//! `MemoryTotpStepStore`).

mod common;

use std::future::Future;
use std::path::PathBuf;

use common::{CONFIG, MIGRATIONS, TEMPLATES, behind_proxy, run};
use id_effect::observability::posture::dangerous_opt_outs;
use id_effect::{Effect, Exit, HNil, caps, from_async, need};
use id_effect_kit::axum::EffectExecutor;
use id_effect_kit::config::VaultAuth;
use id_effect_kit::posture::{AcceptedRisks, CONSOLE_GUARDED, DOCS_CLOSED, Posture, ProbeStatus};
use id_effect_kit::test::http::{StatusCode, TestClient};
use id_effect_kit::test::{
  ContainerError, ContainerImage, Postgres, PostgresContainer, Redis, Vault, VaultContainer,
};
use ledger::config::{self, LedgerConfig};
use ledger::{Assembled, assemble, background, posture, start};

const PASSWORD: &str = "only-vault-knows-this";

#[derive(Debug)]
struct TestError(String);

impl From<ContainerError> for TestError {
  fn from(e: ContainerError) -> Self {
    Self(e.to_string())
  }
}

/// Writes `data` to the KV v2 secret `path` of the dev Vault server.
async fn put_secret(vault: &VaultContainer, path: &str, data: serde_json::Value) {
  let res = reqwest::Client::new()
    .post(format!("{}/v1/secret/data/{path}", vault.addr()))
    .header("x-vault-token", vault.root_token())
    .header("content-type", "application/json")
    .body(serde_json::json!({ "data": data }).to_string())
    .send()
    .await
    .expect("vault reachable");
  assert!(res.status().is_success(), "{}", res.status());
}

/// The repository's `config/` with prod's Vault address pointed at `addr`, in a directory of
/// the test `name`.
fn config_dir_for(addr: &str, name: &str) -> PathBuf {
  let dir = std::env::temp_dir().join(format!("ledger-{name}-{}", std::process::id()));
  std::fs::create_dir_all(&dir).expect("temp dir");
  for file in ["base.toml", "dev.toml", "test.toml", "prod.toml"] {
    let text = std::fs::read_to_string(format!("{CONFIG}/{file}")).expect("profile file");
    std::fs::write(dir.join(file), text.replace("http://vault:8200", addr)).expect("write");
  }
  dir
}

/// Runs `test` with the Ledger assembled from the `prod` profile as `main.rs` assembles it, the
/// risks the profile accepts, and the containers it runs against.
async fn with_prod_ledger<F, Fut>(name: &'static str, test: F)
where
  F: FnOnce(LedgerConfig, Assembled, AcceptedRisks) -> Fut + Send + 'static,
  Fut: Future<Output = ()> + Send + 'static,
{
  if !id_effect_kit::test::require_docker() {
    return;
  }
  let program: Effect<(), TestError, caps!(PostgresContainer, VaultContainer)> =
    need::<PostgresContainer>().flat_map(move |pg| {
      need::<VaultContainer>().flat_map(move |vault| {
        from_async(move || async move {
          let redis = run(Redis::new().start::<ContainerError, HNil>()).await;
          let db = serde_json::json!({"password": PASSWORD, "host": pg.host(), "port": pg.port()});
          put_secret(&vault, "ledger/db", db).await;
          put_secret(
            &vault,
            "ledger/auth",
            serde_json::json!({
              "jwt_secret": "a prod signing secret from vault",
              "cookie_key": "cHJvZHByb2Rwcm9kcHJvZHByb2Rwcm9kcHJvZHByb2Rwcm9kcHJvZHByb2Rwcm9kcHJvZHByb2Rwcm9kcHJvZHByb2Rwcm9kcHJvZHByb2Rwcm9kcHJvZHByb2Rwcm9k",
            }),
          )
          .await;
          let mail = serde_json::json!({"sendgrid_api_key": "SG.from-vault"});
          put_secret(&vault, "ledger/mail", mail).await;

          let dir = config_dir_for(&vault.addr(), name);
          let profiles = config::profiles(dir.clone());
          let profile = profiles.profile("prod").expect("the prod profile");
          let files = profiles.load_for(profile.clone()).expect("the prod files");
          let accepted = AcceptedRisks::from_config(&files).expect("security.accepted_risks");
          let auth = Some(VaultAuth::token(vault.root_token()));
          let mut prod = run(config::load(&profiles, profile, auth)).await;
          // Paths made absolute, Redis at its container: everything else as the profile says.
          prod.db.migrations = MIGRATIONS.to_owned();
          prod.mail.templates = TEMPLATES.to_owned();
          prod.redis.url = Some(redis.url());

          let services = run(start(&prod)).await;
          let _tasks = run(background::tasks(&prod, &services)).await;
          let app = assemble(&prod, services).expect("the prod Ledger assembles");
          test(prod, app, accepted).await;
          let _ = std::fs::remove_dir_all(dir);
          Ok::<(), TestError>(())
        })
      })
    });
  let containers = Postgres::new()
    .user("ledger")
    .password(PASSWORD)
    .database("ledger")
    .layer::<TestError>()
    .merge(Vault::new().layer::<TestError>());
  match EffectExecutor::global()
    .run_effect(program.provide_layer(containers))
    .await
  {
    Exit::Success(()) => {}
    Exit::Failure(id_effect::Cause::Fail(TestError(why))) => panic!("containers: {why}"),
    Exit::Failure(id_effect::Cause::Die(defect)) => panic!("{}", defect.message()),
    Exit::Failure(cause) => panic!("{cause:?}"),
  }
}

#[tokio::test]
async fn ledger_prod_posture_is_clean() {
  with_prod_ledger("posture", |prod, app, accepted| async move {
    assert_eq!(prod.profile, "prod");
    assert_eq!(accepted.iter().count(), 0, "the Ledger accepts no risk");
    let report = posture(
      &prod,
      &app,
      Posture::new(&prod.profile).accepted_risks(accepted),
    )
    .check();
    eprint!("{report}");
    assert!(dangerous_opt_outs().is_empty(), "{report}");
    assert!(report.opt_outs().is_empty(), "{report}");
    for probe in report.probes() {
      assert!(
        matches!(probe.status, ProbeStatus::Pass | ProbeStatus::NotApplicable),
        "{report}"
      );
    }
    // No console and no `/docs` in prod: the probes have nothing to judge.
    for surface in [CONSOLE_GUARDED, DOCS_CLOSED] {
      assert_eq!(
        report.probe(surface).map(|p| p.status),
        Some(ProbeStatus::NotApplicable),
        "{report}"
      );
    }
    assert!(report.passed());
    report.enforce().expect("prod starts");
  })
  .await;
}

#[tokio::test]
async fn m6_ledger_console_disabled_in_prod_profile() {
  // `dev` mounts it, behind `console.admin` (tests/console.rs, under the `test` profile).
  let profiles = config::profiles(CONFIG);
  let dev = run(config::load(
    &profiles,
    profiles.profile("dev").unwrap(),
    None,
  ))
  .await;
  assert!(dev.console.enabled);

  with_prod_ledger("console", |prod, app, _| async move {
    assert!(!prod.console.enabled);
    assert!(
      app
        .routes
        .iter()
        .all(|route| !route.path.starts_with("/_console")),
      "{}",
      app.access_report()
    );
    let client = TestClient::new(behind_proxy(app.router));
    for path in [
      "/_console",
      "/_console/api/fibers",
      "/_console/api/log-level",
      "/_console/api/jobs",
      "/docs",
      "/openapi.json",
    ] {
      let res = run(client.get(path).send()).await;
      assert_eq!(res.status(), StatusCode::NOT_FOUND, "{path}");
    }
    run(client.get("/health").send())
      .await
      .assert_status(StatusCode::OK);
  })
  .await;
}
