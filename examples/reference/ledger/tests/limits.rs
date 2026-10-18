//! The login limits: per organisation (`RateLimitKey::tenant` on the organisation a login names
//! in `X-Ledger-Org`, eco.tenancy #7) next to per client, and both reloaded from the profile file
//! while the service runs (fault.reload #5).

mod common;

use std::time::Duration;

use common::{As, CONFIG, PASSWORD, run, with_ledger};
use id_effect_kit::test::http::StatusCode;
use ledger::config;
use ledger::limits;
use serde_json::json;

#[tokio::test]
async fn an_organisation_shares_one_login_budget_across_clients() {
  with_ledger(
    |config, _| {
      config.auth.login_tenant_burst = 2.0;
      config.auth.login_tenant_per_second = 0.01;
    },
    |l| async move {
      l.team("acme").await;
      l.team("beta").await;
      let login = |user: &'static str, org: &'static str, client: &'static str| {
        let request = l
          .browser(
            l.client.post_json(
              "/auth/login",
              &json!({"username": user, "password": PASSWORD}),
            ),
            None,
          )
          .header(limits::TENANT_HEADER, org)
          .header("x-forwarded-for", client);
        l.send(request)
      };
      // Two sign-ins of acme from two clients use its budget ...
      login("acme-owner", "acme", "198.51.100.1")
        .await
        .assert_status(StatusCode::OK);
      login("acme-viewer", "acme", "198.51.100.2")
        .await
        .assert_status(StatusCode::OK);
      // ... so a third client is limited, whoever it is ...
      let limited = login("acme-accountant", "acme", "198.51.100.3").await;
      limited.assert_status(StatusCode::TOO_MANY_REQUESTS);
      assert!(limited.header("retry-after").is_some());
      // ... while beta has a budget of its own.
      login("beta-owner", "beta", "198.51.100.3")
        .await
        .assert_status(StatusCode::OK);
      // Naming another organisation than the user's does not dodge acme's bucket.
      login("acme-accountant", "beta", "198.51.100.4")
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
      // Without a name only the per-client limit applies.
      l.login("acme-accountant", None).await;
    },
  )
  .await;
}

#[tokio::test]
async fn login_limits_follow_the_profile_file() {
  // A copy of the profiles the test edits while the app runs.
  let dir = std::env::temp_dir().join(format!("ledger-limits-{}", std::process::id()));
  std::fs::create_dir_all(&dir).unwrap();
  for name in ["base.toml", "test.toml"] {
    std::fs::copy(format!("{CONFIG}/{name}"), dir.join(name)).unwrap();
  }
  let profiles = config::profiles(&dir);
  let watch = limits::watch(
    &profiles,
    profiles.profile("test").unwrap(),
    Duration::from_secs(1),
  )
  .expect("the limits load");
  let reload = watch.clone();
  let path = dir.join("test.toml");
  with_ledger(
    move |_, services| services.limits = Some(watch),
    move |l| async move {
      l.team("acme").await;
      let attempt = || {
        l.send(
          l.browser(
            l.client.post_json(
              "/auth/login",
              &json!({"username": "nobody", "password": "wrong password"}),
            ),
            None,
          )
          .header("x-forwarded-for", "203.0.113.9"),
        )
      };
      // The test profile allows 100 attempts per client.
      for _ in 0..3 {
        attempt().await.assert_status(StatusCode::UNAUTHORIZED);
      }
      // Lower it in the file; the next poll picks it up, without a restart.
      let text = std::fs::read_to_string(&path).unwrap();
      let text = text
        .replace("login_burst = 100", "login_burst = 2")
        .replace("login_per_second = 10.0", "login_per_second = 0.01");
      std::fs::write(&path, text).unwrap();
      assert!(
        run(reload.reload_if_changed::<id_effect_kit::config::ConfigError, id_effect::HNil>())
          .await
      );
      assert_eq!(reload.generation(), 1);
      for _ in 0..2 {
        attempt().await.assert_status(StatusCode::UNAUTHORIZED);
      }
      attempt().await.assert_status(StatusCode::TOO_MANY_REQUESTS);
      // A file that no longer parses keeps the limits in force.
      std::fs::write(&path, "[auth\nlogin_burst = ").unwrap();
      let rejected = id_effect_kit::axum::EffectExecutor::global()
        .run_effect(
          reload.reload_if_changed::<id_effect_kit::config::ConfigError, id_effect::HNil>(),
        )
        .await;
      assert!(rejected.into_result().is_err());
      assert!(reload.last_error().is_some());
      attempt().await.assert_status(StatusCode::TOO_MANY_REQUESTS);
      l.get("/health", &As::Anonymous)
        .await
        .assert_status(StatusCode::OK);
      let _ = std::fs::remove_dir_all(path.parent().unwrap());
    },
  )
  .await;
}
