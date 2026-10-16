//! `VaultSettings` against a real Vault dev server in a container (testcontainers).
//!
//! Without Docker the test prints `SKIP` and passes, unless `ID_EFFECT_REQUIRE_DOCKER=1` (the CI
//! `containers-integration` job), where a missing Docker daemon fails it.

#![cfg(feature = "vault")]

use std::time::Duration;

use id_effect::{Cause, Effect, Exit, HNil};
use id_effect_config::{
  Config, ConfigEnv, ConfigError, Secret, VaultAuth, VaultErrorKind, VaultProvider, VaultSettings,
};
use id_effect_tokio::TokioRuntime;
use testcontainers::core::{IntoContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};

const ROOT_TOKEN: &str = "root-token";

async fn run<A, E>(eff: Effect<A, E>) -> Result<A, E>
where
  A: Send + 'static,
  E: Send + std::fmt::Debug + 'static,
{
  match TokioRuntime::current().expect("tokio").run(eff).await {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
  }
}

/// A Vault dev server (KV v2 at `secret/`), or `None` (after printing SKIP) without Docker.
async fn vault_dev_server() -> Option<(ContainerAsync<GenericImage>, String)> {
  let started = GenericImage::new("hashicorp/vault", "1.20")
    .with_exposed_port(8200.tcp())
    .with_wait_for(WaitFor::message_on_stdout(
      "Development mode should NOT be used",
    ))
    .with_env_var("VAULT_DEV_ROOT_TOKEN_ID", ROOT_TOKEN)
    .with_env_var("SKIP_SETCAP", "true")
    .start()
    .await;
  let container = match started {
    Ok(container) => container,
    Err(e) if std::env::var("ID_EFFECT_REQUIRE_DOCKER").as_deref() == Ok("1") => {
      panic!("ID_EFFECT_REQUIRE_DOCKER=1 but the Vault container did not start: {e}")
    }
    Err(e) => {
      eprintln!("SKIP vault_container: Docker is not available ({e})");
      return None;
    }
  };
  let host = container.get_host().await.expect("host");
  let port = container.get_host_port_ipv4(8200).await.expect("port");
  Some((container, format!("http://{host}:{port}")))
}

/// `POST <addr>/v1/<route>` with the root token.
async fn vault_post(addr: &str, route: &str, body: serde_json::Value) {
  let response = reqwest::Client::new()
    .post(format!("{addr}/v1/{route}"))
    .header("X-Vault-Token", ROOT_TOKEN)
    .body(body.to_string())
    .send()
    .await
    .expect("vault request");
  assert!(
    response.status().is_success(),
    "POST {route}: {}",
    response.status()
  );
}

fn read(key: &'static str) -> Effect<String, ConfigError, ConfigEnv> {
  Config::string(key).run::<String, ConfigError, _, _>()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vault_dev_server_round_trip() {
  let Some((_container, addr)) = vault_dev_server().await else {
    return;
  };
  vault_post(
    &addr,
    "secret/data/ledger/db",
    serde_json::json!({ "data": { "password": "s3cr3t", "port": 5432 } }),
  )
  .await;

  // The shorthand layer on the default `secret/` mount, read through `Config`.
  let layer = VaultProvider::layer::<ConfigError, _, _>(
    addr.clone(),
    VaultAuth::token(ROOT_TOKEN),
    ["ledger/db"],
  );
  let password = Config::string("password")
    .secret()
    .run::<Secret<String>, ConfigError, ConfigEnv, _>()
    .provide_layer(layer);
  let password = run(password).await.expect("password");
  assert_eq!(password.expose(), "s3cr3t");
  assert_eq!(format!("{password:?}"), "<redacted>");

  // A second KV v2 engine on a custom mount path, nested under a key.
  vault_post(
    &addr,
    "sys/mounts/team-a/kv",
    serde_json::json!({ "type": "kv", "options": { "version": "2" } }),
  )
  .await;
  vault_post(
    &addr,
    "team-a/kv/data/api",
    serde_json::json!({ "data": { "key": "v1" } }),
  )
  .await;
  let settings = VaultSettings::new(addr.clone(), VaultAuth::token(ROOT_TOKEN))
    .mount("team-a/kv")
    .path_at("api", "api");
  let vault = run(settings.fetch::<ConfigError, HNil>())
    .await
    .expect("fetch");
  assert_eq!(vault.secret::<String>("api.key").unwrap().expose(), "v1");

  // Rotate, then refresh: the new version is served.
  vault_post(
    &addr,
    "team-a/kv/data/api",
    serde_json::json!({ "data": { "key": "v2" } }),
  )
  .await;
  run(vault.refresh::<ConfigError, HNil>())
    .await
    .expect("refresh");
  assert_eq!(vault.generation(), 2);
  assert_eq!(vault.secret::<String>("api.key").unwrap().expose(), "v2");

  // The refresh fiber of a layer picks up a rotation without a restart.
  let refreshing = settings
    .refresh_every(Duration::from_millis(100))
    .layer::<ConfigError>();
  let rotated_addr = addr.clone();
  let program = read("api.key").flat_map(move |before| {
    id_effect::from_async(move || async move {
      vault_post(
        &rotated_addr,
        "team-a/kv/data/api",
        serde_json::json!({ "data": { "key": "v3" } }),
      )
      .await;
      tokio::time::sleep(Duration::from_millis(600)).await;
      Ok::<_, ConfigError>(before)
    })
    .flat_map(|before| read("api.key").map(move |after| (before, after)))
  });
  let (before, after) = run(program.provide_layer(refreshing))
    .await
    .expect("rotation");
  assert_eq!((before.as_str(), after.as_str()), ("v2", "v3"));

  // A bad token and a missing secret are typed `ConfigError::Vault` status errors.
  let denied = VaultSettings::new(addr.clone(), VaultAuth::token("wrong")).path("ledger/db");
  assert!(matches!(
    run(denied.fetch::<ConfigError, HNil>()).await,
    Err(ConfigError::Vault {
      kind: VaultErrorKind::Status(403),
      ..
    })
  ));
  let missing = VaultSettings::new(addr, VaultAuth::token(ROOT_TOKEN)).path("no/such/secret");
  assert!(matches!(
    run(missing.fetch::<ConfigError, HNil>()).await,
    Err(ConfigError::Vault { kind: VaultErrorKind::Status(404), path, .. }) if path == "no/such/secret"
  ));
}
