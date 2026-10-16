//! Unit tests against a mocked Vault HTTP API (wiremock). The real dev server is exercised by
//! `tests/vault_container.rs`.

use super::*;
use crate::Config;
use ::id_effect::{Cause, Exit};
use id_effect_tokio::TokioRuntime;
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Runs `eff` on the ambient tokio runtime (reqwest needs its IO driver).
async fn run<A, E>(eff: Effect<A, E>) -> Result<A, E>
where
  A: Send + 'static,
  E: Send + fmt::Debug + 'static,
{
  match TokioRuntime::current().expect("tokio").run(eff).await {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
  }
}

fn kv2(data: Value) -> ResponseTemplate {
  ResponseTemplate::new(200).set_body_json(json!({
    "request_id": "r",
    "data": { "data": data, "metadata": { "version": 1 } }
  }))
}

async fn mount_secret(server: &MockServer, route: &str, response: ResponseTemplate) {
  Mock::given(method("GET"))
    .and(path(route))
    .respond_with(response)
    .mount(server)
    .await;
}

fn settings(server: &MockServer) -> VaultSettings {
  VaultSettings::new(server.uri(), VaultAuth::token("t0ken"))
}

async fn fetch(settings: VaultSettings) -> Result<VaultProvider, ConfigError> {
  run(settings.fetch::<ConfigError, HNil>()).await
}

#[tokio::test]
async fn fetch_merges_paths_with_headers_mount_and_namespace() {
  let server = MockServer::start().await;
  Mock::given(method("GET"))
    .and(path("/v1/team-a/kv/data/ledger/app"))
    .and(header(TOKEN_HEADER, "t0ken"))
    .and(header(NAMESPACE_HEADER, "admin/team-a"))
    .respond_with(kv2(
      json!({ "port": 8080, "name": "ledger", "debug": true }),
    ))
    .expect(1)
    .mount(&server)
    .await;
  Mock::given(method("GET"))
    .and(path("/v1/team-a/kv/data/ledger/db"))
    .respond_with(kv2(json!({ "password": "hunter2", "port": 5432 })))
    .expect(1)
    .mount(&server)
    .await;

  let vault = fetch(
    settings(&server)
      .mount("/team-a/kv/")
      .namespace("admin/team-a")
      .path("ledger/app")
      .path_at("/ledger/db", "db"),
  )
  .await
  .expect("fetch");

  assert_eq!(vault.generation(), 1);
  assert_eq!(vault.last_refresh_error(), None);
  assert_eq!(vault.settings().kv_mount(), "/team-a/kv/");
  assert_eq!(Config::integer("port").load(&vault).unwrap(), 8080);
  assert_eq!(Config::string("name").load(&vault).unwrap(), "ledger");
  assert_eq!(Config::integer("db.port").load(&vault).unwrap(), 5432);
  let password = Config::string("db.password").secret().load(&vault).unwrap();
  assert_eq!(password.expose(), "hunter2");
  assert_eq!(format!("{password:?}"), "<redacted>");

  let typed: Secret<String> = vault.secret("db.password").unwrap();
  assert_eq!(typed.expose(), "hunter2");
  assert!(*vault.secret::<bool>("debug").unwrap().expose());

  let debug = format!("{vault:?}");
  assert!(debug.contains("ledger/db") && debug.contains("generation: 1"));
  assert!(!debug.contains("hunter2") && !debug.contains("t0ken"));
  assert!(!format!("{:?}", vault.settings()).contains("t0ken"));
}

#[tokio::test]
async fn later_paths_override_earlier_ones() {
  let server = MockServer::start().await;
  mount_secret(
    &server,
    "/v1/secret/data/a",
    kv2(json!({ "x": "a", "y": "a" })),
  )
  .await;
  mount_secret(&server, "/v1/secret/data/b", kv2(json!({ "y": "b" }))).await;
  let vault = fetch(settings(&server).paths(["a", "b"])).await.unwrap();
  assert_eq!(Config::string("x").load(&vault).unwrap(), "a");
  assert_eq!(Config::string("y").load(&vault).unwrap(), "b");
  assert_eq!(Config::string("z").load(&vault).ok(), None);
}

#[tokio::test]
async fn secret_reports_missing_and_redacts_wrong_types() {
  let server = MockServer::start().await;
  mount_secret(
    &server,
    "/v1/secret/data/s",
    kv2(json!({ "token": "s3cr3t" })),
  )
  .await;
  let vault = fetch(settings(&server).path("s")).await.unwrap();
  assert!(matches!(
    vault.secret::<String>("nope"),
    Err(ConfigError::Missing { path }) if path == "nope"
  ));
  match vault.secret::<i64>("token") {
    Err(err @ ConfigError::Invalid { .. }) => assert!(!err.to_string().contains("s3cr3t")),
    other => panic!("expected Invalid, got {other:?}"),
  }
}

#[tokio::test]
async fn status_errors_carry_vault_messages() {
  let server = MockServer::start().await;
  mount_secret(
    &server,
    "/v1/secret/data/denied",
    ResponseTemplate::new(403).set_body_json(json!({ "errors": ["permission denied"] })),
  )
  .await;
  mount_secret(&server, "/v1/secret/data/gone", ResponseTemplate::new(404)).await;

  match fetch(settings(&server).path("denied")).await {
    Err(ConfigError::Vault { path, kind, reason }) => {
      assert_eq!(path, "denied");
      assert_eq!(kind, VaultErrorKind::Status(403));
      assert_eq!(reason, "permission denied");
    }
    other => panic!("expected a Vault error, got {other:?}"),
  }
  assert!(matches!(
    fetch(settings(&server).path("gone")).await,
    Err(ConfigError::Vault { kind: VaultErrorKind::Status(404), reason, .. }) if reason == "Not Found"
  ));
}

#[tokio::test]
async fn malformed_bodies_are_decode_errors() {
  let server = MockServer::start().await;
  mount_secret(
    &server,
    "/v1/secret/data/html",
    ResponseTemplate::new(200).set_body_string("<html>"),
  )
  .await;
  mount_secret(
    &server,
    "/v1/secret/data/kv1",
    ResponseTemplate::new(200).set_body_json(json!({ "data": { "password": "x" } })),
  )
  .await;
  for (secret, needle) in [("html", "invalid JSON"), ("kv1", "KV version 2")] {
    match fetch(settings(&server).path(secret)).await {
      Err(ConfigError::Vault {
        kind: VaultErrorKind::Decode,
        reason,
        ..
      }) => assert!(reason.contains(needle), "{reason}"),
      other => panic!("expected a decode error, got {other:?}"),
    }
  }
}

#[tokio::test]
async fn unreachable_server_is_a_transport_error() {
  // Bind then drop a listener: nothing accepts on that port any more.
  let addr = std::net::TcpListener::bind("127.0.0.1:0")
    .unwrap()
    .local_addr()
    .unwrap();
  let vault = VaultSettings::new(format!("http://{addr}"), VaultAuth::token("t"))
    .timeout(Duration::from_secs(2))
    .path("p");
  assert!(matches!(
    fetch(vault).await,
    Err(ConfigError::Vault { kind: VaultErrorKind::Transport, path, .. }) if path == "p"
  ));
}

#[tokio::test]
async fn invalid_settings_fail_before_any_request() {
  let auth = || VaultAuth::token("t");
  let cases = [
    (
      VaultSettings::new("not a url", auth()).path("p"),
      "vault.addr",
    ),
    (
      VaultSettings::new("ftp://vault", auth()).path("p"),
      "vault.addr",
    ),
    (VaultSettings::new("http://vault", auth()), "vault.paths"),
    (
      VaultSettings::new("http://vault", auth())
        .path("p")
        .refresh_every(Duration::ZERO),
      "vault.refresh_every",
    ),
  ];
  for (settings, expected) in cases {
    match fetch(settings).await {
      Err(ConfigError::Invalid { path, .. }) => assert_eq!(path, expected),
      other => panic!("expected Invalid at {expected}, got {other:?}"),
    }
  }
}

#[test]
fn secret_url_joins_addr_mount_and_path() {
  let s = VaultSettings::new("http://vault:8200/", VaultAuth::token("t"));
  assert_eq!(s.addr(), "http://vault:8200/");
  assert_eq!(s.kv_mount(), DEFAULT_MOUNT);
  assert_eq!(
    s.secret_url("/app/db/"),
    "http://vault:8200/v1/secret/data/app/db"
  );
  assert_eq!(
    s.mount("ns/kv").secret_url("x"),
    "http://vault:8200/v1/ns/kv/data/x"
  );
}

#[test]
fn token_auth_reads_the_environment() {
  temp_env::with_var(TOKEN_ENV_VAR, Some("from-env"), || {
    let VaultAuth::Token(token) = VaultAuth::token_from_env().unwrap();
    assert_eq!(token.expose(), "from-env");
    assert_eq!(
      format!("{:?}", VaultAuth::token_from_env().unwrap()),
      "Token(<redacted>)"
    );
  });
  for value in [None, Some("")] {
    temp_env::with_var("IDCFG_VAULT_TOKEN_UNSET", value, || {
      assert!(matches!(
        VaultAuth::token_from_var("IDCFG_VAULT_TOKEN_UNSET"),
        Err(ConfigError::Missing { path }) if path == "IDCFG_VAULT_TOKEN_UNSET"
      ));
    });
  }
}

#[cfg(unix)]
#[test]
fn token_auth_rejects_non_utf8() {
  use std::os::unix::ffi::OsStrExt;
  let bad = std::ffi::OsStr::from_bytes(&[0x74, 0xff]);
  temp_env::with_var("IDCFG_VAULT_TOKEN_UTF8", Some(bad), || {
    assert!(matches!(
      VaultAuth::token_from_var("IDCFG_VAULT_TOKEN_UTF8"),
      Err(ConfigError::InvalidUtf8 { var }) if var == "IDCFG_VAULT_TOKEN_UTF8"
    ));
  });
}

#[tokio::test]
async fn refresh_swaps_on_success_and_keeps_the_snapshot_on_failure() {
  let server = MockServer::start().await;
  Mock::given(method("GET"))
    .and(path("/v1/secret/data/rot"))
    .respond_with(kv2(json!({ "password": "v1" })))
    .up_to_n_times(1)
    .mount(&server)
    .await;
  let vault = fetch(settings(&server).path("rot")).await.unwrap();
  assert_eq!(vault.secret::<String>("password").unwrap().expose(), "v1");

  // Nothing mounted any more: wiremock answers 404.
  let failed = run(vault.refresh::<ConfigError, HNil>()).await;
  assert!(matches!(
    failed,
    Err(ConfigError::Vault {
      kind: VaultErrorKind::Status(404),
      ..
    })
  ));
  assert_eq!(vault.generation(), 1);
  assert!(vault.last_refresh_error().unwrap().contains("HTTP 404"));
  assert_eq!(vault.secret::<String>("password").unwrap().expose(), "v1");

  mount_secret(
    &server,
    "/v1/secret/data/rot",
    kv2(json!({ "password": "v2" })),
  )
  .await;
  run(vault.refresh::<ConfigError, HNil>()).await.unwrap();
  assert_eq!(vault.generation(), 2);
  assert_eq!(vault.last_refresh_error(), None);
  assert_eq!(vault.secret::<String>("password").unwrap().expose(), "v2");
  assert_eq!(Config::string("password").load(&vault).unwrap(), "v2");
}

#[tokio::test]
async fn figment_provider_exposes_the_snapshot() {
  let server = MockServer::start().await;
  mount_secret(
    &server,
    "/v1/secret/data/db",
    kv2(json!({ "password": "pw" })),
  )
  .await;
  let vault = fetch(settings(&server).path_at("db", "db")).await.unwrap();
  assert_eq!(
    ::figment::Provider::metadata(&vault).name,
    "HashiCorp Vault"
  );
  #[derive(serde::Deserialize)]
  struct Db {
    password: Secret<String>,
  }
  let db: Db = Figment::from(vault.clone()).extract_inner("db").unwrap();
  assert_eq!(db.password.expose(), "pw");
  assert!(vault.figment().contains("db.password"));
}

/// Program reading `password` through the installed configuration capability.
fn read_password() -> Effect<String, ConfigError, ConfigEnv> {
  Config::string("password").run::<String, ConfigError, _, _>()
}

#[tokio::test]
async fn shorthand_layer_installs_the_provider() {
  let server = MockServer::start().await;
  mount_secret(
    &server,
    "/v1/secret/data/app",
    kv2(json!({ "password": "pw" })),
  )
  .await;
  let layer =
    VaultProvider::layer::<ConfigError, _, _>(server.uri(), VaultAuth::token("t"), ["app"]);
  let got = run(read_password().provide_layer(layer)).await.unwrap();
  assert_eq!(got, "pw");

  let failing =
    VaultProvider::layer::<ConfigError, _, _>(server.uri(), VaultAuth::token("t"), ["none"]);
  assert!(matches!(
    run(read_password().provide_layer(failing)).await,
    Err(ConfigError::Vault { .. })
  ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn layer_refreshes_while_alive_and_stops_when_the_scope_closes() {
  let server = MockServer::start().await;
  Mock::given(method("GET"))
    .and(path("/v1/secret/data/rot"))
    .respond_with(kv2(json!({ "password": "old" })))
    .up_to_n_times(1)
    .mount(&server)
    .await;
  Mock::given(method("GET"))
    .and(path("/v1/secret/data/rot"))
    .respond_with(kv2(json!({ "password": "new" })))
    .mount(&server)
    .await;
  let layer = settings(&server)
    .path("rot")
    .refresh_every(Duration::from_millis(20))
    .layer::<ConfigError>();

  // Read once, wait for a refresh, read again: inside one scope.
  let program = read_password().flat_map(|first| {
    sleep::<ConfigError, ConfigEnv>(Duration::from_millis(300))
      .flat_map(move |()| read_password().map(move |second| (first, second)))
  });
  let (first, second) = run(program.provide_layer(layer)).await.unwrap();
  assert_eq!((first.as_str(), second.as_str()), ("old", "new"));

  // The scope is closed: the refresh fiber no longer polls Vault.
  tokio::time::sleep(Duration::from_millis(100)).await;
  let settled = server.received_requests().await.unwrap().len();
  tokio::time::sleep(Duration::from_millis(200)).await;
  assert_eq!(server.received_requests().await.unwrap().len(), settled);
}

#[cfg(all(feature = "toml", feature = "env"))]
#[test]
fn profiles_put_vault_above_files_and_below_env() {
  let dir = tempfile::tempdir().unwrap();
  std::fs::write(
    dir.path().join("base.toml"),
    "[db]\nhost = \"localhost\"\npassword = \"from-base\"\nuser = \"app\"\n",
  )
  .unwrap();
  std::fs::write(
    dir.path().join("prod.toml"),
    "[db]\nhost = \"db.internal\"\npassword = \"from-prod-file\"\n",
  )
  .unwrap();
  let profiles = Profiles::new()
    .dir(dir.path())
    .env_var("IDCFG_VAULT_PROFILE")
    .env_prefix("IDCFG_VAULT_APP_");

  let vars = [
    ("IDCFG_VAULT_PROFILE", Some("prod")),
    ("IDCFG_VAULT_APP_DB__USER", Some("from-env")),
  ];
  temp_env::with_vars(vars, || {
    let rt = TokioRuntime::new_multi_thread().unwrap();
    rt.block_on(async {
      let server = MockServer::start().await;
      mount_secret(
        &server,
        "/v1/secret/data/ledger/db",
        kv2(json!({ "password": "from-vault", "user": "from-vault" })),
      )
      .await;
      let vault = fetch(
        settings(&server)
          .path_at("ledger/db", "db")
          .profiles(profiles.clone()),
      )
      .await
      .unwrap();
      let get = |key: &str| Config::string(key).load(&vault).unwrap();
      assert_eq!(get("db.host"), "db.internal");
      assert_eq!(get("db.password"), "from-vault");
      assert_eq!(get("db.user"), "from-env");
      // The secret alone, without files or environment.
      assert_eq!(
        vault.secret::<String>("db.user").unwrap().expose(),
        "from-vault"
      );
      assert!(!vault.figment().contains("db.host"));

      // A refresh re-reads the profile files with the secrets.
      std::fs::write(
        dir.path().join("prod.toml"),
        "[db]\nhost = \"db2.internal\"\n",
      )
      .unwrap();
      run(vault.refresh::<ConfigError, HNil>()).await.unwrap();
      assert_eq!(get("db.host"), "db2.internal");

      // An undeclared profile fails the fetch.
      temp_env::with_var("IDCFG_VAULT_PROFILE", Some("staging"), || {
        assert!(matches!(
          Base::resolve(&settings(&server).path("x").profiles(profiles.clone())),
          Err(ConfigError::UnknownProfile { .. })
        ));
      });
    });
  });
}

#[test]
fn vault_errors_reads_only_non_empty_lists() {
  assert_eq!(
    vault_errors(br#"{"errors":["a","b"]}"#).as_deref(),
    Some("a; b")
  );
  assert_eq!(vault_errors(br#"{"errors":[]}"#), None);
  assert_eq!(vault_errors(br#"{"warnings":["w"]}"#), None);
  assert_eq!(vault_errors(b"oops"), None);
}
