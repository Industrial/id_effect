//! Profiles: `dev`, `test` and `prod` select their settings; `prod` reads the database password
//! and the token secret from Vault and connects with them.

mod common;

use std::path::PathBuf;

use common::{CONFIG, run};
use id_effect::{Effect, Exit, caps, from_async, need};
use id_effect_kit::axum::EffectExecutor;
use id_effect_kit::config::{ConfigError, VaultAuth};
use id_effect_kit::test::{
  ContainerError, ContainerImage, Postgres, PostgresContainer, Vault, VaultContainer,
};
use ledger::Database;
use ledger::config::{self, LedgerConfig};

async fn load(
  profile: &str,
  dir: &str,
  vault: Option<VaultAuth>,
) -> Result<LedgerConfig, ConfigError> {
  let profiles = config::profiles(dir);
  let profile = profiles.profile(profile)?;
  match EffectExecutor::global()
    .run_effect(config::load(&profiles, profile, vault))
    .await
  {
    Exit::Success(config) => Ok(config),
    Exit::Failure(id_effect::Cause::Fail(e)) => Err(e),
    Exit::Failure(cause) => panic!("{cause:?}"),
  }
}

#[tokio::test]
async fn profiles_select_their_settings() {
  let dev = load("dev", CONFIG, None).await.expect("dev");
  assert_eq!((dev.http.host.as_str(), dev.http.port), ("127.0.0.1", 8080));
  assert_eq!(dev.auth.session_cookie, "id_session");
  assert!(!dev.auth.secure_cookies);
  assert!(dev.redis.url.is_some() && dev.oidc.is_some() && dev.vault.is_none());
  // Secrets never print.
  assert!(!format!("{dev:?}").contains("ledger-dev"), "{dev:?}");

  let test = load("test", CONFIG, None).await.expect("test");
  assert_eq!(test.auth.password_memory_kib, 64);
  assert!(test.redis.url.is_none() && test.oidc.is_none());

  // prod has no secret in its files: without Vault it does not load.
  assert!(
    load("prod", CONFIG, Some(VaultAuth::token("t")))
      .await
      .is_err()
  );
  let unknown = config::profiles(CONFIG).select(["--profile", "staging"], None);
  assert!(matches!(unknown, Err(ConfigError::UnknownProfile { .. })));
}

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

/// The repository's `config/` with prod's Vault address pointed at `addr`.
fn config_dir_for(addr: &str) -> PathBuf {
  let dir = std::env::temp_dir().join(format!("ledger-config-{}", std::process::id()));
  std::fs::create_dir_all(&dir).expect("temp dir");
  for name in ["base.toml", "dev.toml", "test.toml", "prod.toml"] {
    let text = std::fs::read_to_string(format!("{CONFIG}/{name}")).expect("profile file");
    let text = text.replace("http://vault:8200", addr);
    std::fs::write(dir.join(name), text).expect("write profile");
  }
  dir
}

#[tokio::test]
async fn prod_reads_its_database_password_from_vault() {
  if !id_effect_kit::test::require_docker() {
    return;
  }
  const PASSWORD: &str = "only-vault-knows-this";
  let program: Effect<(), TestError, caps!(PostgresContainer, VaultContainer)> =
    need::<PostgresContainer>().flat_map(|pg| {
      need::<VaultContainer>().flat_map(move |vault| {
        from_async(move || async move {
          put_secret(
            &vault,
            "ledger/db",
            serde_json::json!({"password": PASSWORD, "host": pg.host(), "port": pg.port()}),
          )
          .await;
          put_secret(
            &vault,
            "ledger/auth",
            serde_json::json!({"jwt_secret": "a prod signing secret from vault"}),
          )
          .await;
          let dir = config_dir_for(&vault.addr());
          let auth = Some(VaultAuth::token(vault.root_token()));
          let prod = load("prod", dir.to_str().unwrap(), auth)
            .await
            .expect("prod");
          assert_eq!(prod.db.password.expose(), PASSWORD);
          assert_eq!(
            prod.auth.jwt_secret.expose(),
            "a prod signing secret from vault"
          );
          assert!(prod.auth.secure_cookies && prod.auth.session_cookie.starts_with("__Host-"));

          // The password from Vault opens the database.
          let database = run(Database::connect(&prod.db)).await;
          let rows = run(database.primary.query("SELECT 1 AS up", &[])).await;
          assert_eq!(rows[0].get::<i32>("up"), Ok(1));
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
