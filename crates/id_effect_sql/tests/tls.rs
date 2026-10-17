//! TLS to PostgreSQL 17 and MySQL 8.4 (features `pg` / `mysql`): each test generates a throwaway
//! CA and a server certificate for `localhost` (rcgen), starts a server with them, and checks
//! that verified connections succeed and report TLS in use, that a wrong CA or host name fails,
//! that the explicit config overrides the URL, and that `require` refuses a server without TLS
//! while `disable` still connects to it.
//!
//! The servers run in containers (testcontainers; nextest runs every test in a process of its
//! own). Without Docker the tests print `SKIP` and return, unless `ID_EFFECT_REQUIRE_DOCKER=1`
//! (the CI `containers-integration` job), which turns a missing Docker daemon into a failure.

#![cfg(any(feature = "pg", feature = "mysql"))]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use id_effect::{Cause, Effect, Exit};
use id_effect_sql::{SqlError, SqlValue};
use id_effect_tokio::TokioRuntime;
use rcgen::{
  BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair,
  KeyUsagePurpose,
};

const PASSWORD: &str = "tls-test";

/// A CA, a server certificate it issued for `localhost`, and an unrelated CA.
struct Pki {
  ca_pem: String,
  server_cert_pem: String,
  server_key_pem: String,
  other_ca_pem: String,
}

fn ca(name: &str) -> (rcgen::Certificate, KeyPair) {
  let key = KeyPair::generate().expect("CA key");
  let mut params = CertificateParams::new(Vec::<String>::new()).expect("CA params");
  params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
  params.distinguished_name.push(DnType::CommonName, name);
  params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
  (params.self_signed(&key).expect("CA certificate"), key)
}

fn pki() -> Pki {
  let (ca_cert, ca_key) = ca("id_effect_sql test CA");
  let key = KeyPair::generate().expect("server key");
  let mut params = CertificateParams::new(vec!["localhost".to_owned()]).expect("server params");
  params
    .distinguished_name
    .push(DnType::CommonName, "localhost");
  params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
  params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
  let server = params
    .signed_by(&key, &ca_cert, &ca_key)
    .expect("server certificate");
  Pki {
    ca_pem: ca_cert.pem(),
    server_cert_pem: server.pem(),
    server_key_pem: key.serialize_pem(),
    other_ca_pem: ca("another CA").0.pem(),
  }
}

/// `pem` in a file of its own (the URL forms take a path).
fn pem_file(test: &str, pem: &str) -> PathBuf {
  let path = std::env::temp_dir().join(format!("{test}-{}-ca.pem", std::process::id()));
  std::fs::write(&path, pem).expect("write CA file");
  path
}

fn unavailable(test: &str, why: &str) {
  assert!(
    std::env::var("ID_EFFECT_REQUIRE_DOCKER").as_deref() != Ok("1"),
    "{test}: ID_EFFECT_REQUIRE_DOCKER=1 but the server is unavailable: {why}"
  );
  eprintln!("SKIP {test}: {why} (start Docker)");
}

async fn run<A: Send + 'static>(eff: Effect<A, SqlError>) -> Result<A, SqlError> {
  match TokioRuntime::current().expect("tokio").run(eff).await {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
  }
}

/// A failed connection: `QueryFailed` without SQL, its message containing `cause`.
#[track_caller]
fn assert_refused<T>(result: Result<T, SqlError>, cause: &str) {
  match result {
    Err(SqlError::QueryFailed { sql, message }) if sql.is_empty() && message.contains(cause) => {}
    Err(other) => panic!("expected a connection failure containing {cause:?}, got {other:?}"),
    Ok(_) => panic!("expected a connection failure containing {cause:?}, but it connected"),
  }
}

/// Retries `connect` until it succeeds: the server inside a started container may still be
/// starting up.
async fn wait_until_ready<T, F, Fut>(connect: F) -> Result<T, String>
where
  F: Fn() -> Fut,
  Fut: std::future::Future<Output = Result<T, SqlError>>,
{
  let deadline = Instant::now() + Duration::from_secs(90);
  loop {
    match connect().await {
      Ok(v) => return Ok(v),
      Err(e) if Instant::now() > deadline => return Err(e.to_string()),
      Err(_) => tokio::time::sleep(Duration::from_millis(250)).await,
    }
  }
}

#[cfg(feature = "pg")]
mod pg {
  use id_effect_sql::SqlClient;
  use id_effect_sql::pg::{PgPoolConfig, PgSqlClient, PgTlsMode, pg_pool_from_config};
  use testcontainers::core::{IntoContainerPort, Mount, WaitFor};
  use testcontainers::runners::AsyncRunner;
  use testcontainers::{ContainerAsync, GenericImage, ImageExt};

  use super::*;

  /// PostgreSQL 17 on a tmpfs data directory, with TLS on when `pki` is given. The key is copied
  /// in world-readable, so the entrypoint is wrapped to install it `0600` owned by `postgres`
  /// (the server refuses a key others can read).
  async fn start(pki: Option<&Pki>) -> Result<(ContainerAsync<GenericImage>, String), String> {
    let mut image = GenericImage::new("postgres", "17")
      .with_exposed_port(5432.tcp())
      .with_wait_for(WaitFor::message_on_stdout(
        "PostgreSQL init process complete; ready for start up.",
      ));
    if pki.is_some() {
      image = image.with_entrypoint("sh");
    }
    let mut request = image
      .with_env_var("POSTGRES_PASSWORD", PASSWORD)
      .with_mount(Mount::tmpfs_mount("/var/lib/postgresql/data"))
      .with_startup_timeout(Duration::from_secs(180));
    if let Some(pki) = pki {
      request = request
        .with_copy_to("/certs/server.crt", pki.server_cert_pem.clone().into_bytes())
        .with_copy_to("/certs/server.key", pki.server_key_pem.clone().into_bytes())
        .with_cmd([
          "-c",
          "install -o postgres -g postgres -m 0600 /certs/server.key /var/lib/postgresql/server.key \
           && exec docker-entrypoint.sh postgres -c ssl=on \
           -c ssl_cert_file=/certs/server.crt -c ssl_key_file=/var/lib/postgresql/server.key",
        ]);
    }
    let container = request.start().await.map_err(|e| e.to_string())?;
    let host = container.get_host().await.map_err(|e| e.to_string())?;
    let port = container
      .get_host_port_ipv4(5432)
      .await
      .map_err(|e| e.to_string())?;
    Ok((container, format!("{host}:{port}")))
  }

  fn url(addr: &str) -> String {
    format!("postgres://postgres:{PASSWORD}@{addr}/postgres")
  }

  /// Whether the session of a pool built from `config` is encrypted (`pg_stat_ssl`).
  async fn session_ssl(config: PgPoolConfig) -> Result<bool, SqlError> {
    let pool = pg_pool_from_config(config.with_max_size(1))
      .await
      .map_err(|e| e.0)?;
    let rows = run(PgSqlClient::new(pool.clone()).query(
      "SELECT ssl FROM pg_stat_ssl WHERE pid = pg_backend_pid()",
      &[],
    ))
    .await?;
    pool.close().await;
    match rows[0].values() {
      [SqlValue::Bool(ssl)] => Ok(*ssl),
      other => panic!("pg_stat_ssl.ssl: {other:?}"),
    }
  }

  #[tokio::test]
  async fn pg_verify_full_with_the_right_ca_is_encrypted() {
    let t = "pg_verify_full_with_the_right_ca_is_encrypted";
    let pki = pki();
    let (_container, addr) = match start(Some(&pki)).await {
      Ok(started) => started,
      Err(e) => return unavailable(t, &format!("the PostgreSQL container did not start: {e}")),
    };
    let verified = || {
      PgPoolConfig::from_url(url(&addr))
        .with_tls_mode(PgTlsMode::VerifyFull)
        .with_root_cert_pem(pki.ca_pem.clone())
    };
    assert!(
      wait_until_ready(|| session_ssl(verified()))
        .await
        .expect("verify-full connects")
    );

    // The URL form: `sslmode` and `sslrootcert` (a file) are honoured.
    let ca_file = pem_file(t, &pki.ca_pem);
    let from_url = format!(
      "{}?sslmode=verify-full&sslrootcert={}",
      url(&addr),
      ca_file.display()
    );
    let config = PgPoolConfig::from_url(&from_url);
    assert_eq!(config.effective_tls_mode().unwrap(), PgTlsMode::VerifyFull);
    assert_eq!(session_ssl(config).await, Ok(true));
    // A root certificate file given to the config works the same.
    let config = PgPoolConfig::from_url(url(&addr))
      .with_tls_mode(PgTlsMode::VerifyFull)
      .with_root_cert_file(&ca_file);
    assert_eq!(session_ssl(config).await, Ok(true));

    // A CA that did not issue the server certificate: no connection.
    let wrong_ca = PgPoolConfig::from_url(url(&addr))
      .with_tls_mode(PgTlsMode::VerifyFull)
      .with_root_cert_pem(pki.other_ca_pem.clone());
    assert_refused(session_ssl(wrong_ca).await, "invalid peer certificate");
    // Nor without one: a private CA is not in the Mozilla roots.
    let no_ca = PgPoolConfig::from_url(url(&addr)).with_tls_mode(PgTlsMode::VerifyFull);
    assert_refused(session_ssl(no_ca).await, "invalid peer certificate");
    // The explicit root certificate replaces the URL's.
    let overridden = PgPoolConfig::from_url(&from_url).with_root_cert_pem(pki.other_ca_pem.clone());
    assert_refused(session_ssl(overridden).await, "invalid peer certificate");

    // The certificate names `localhost` only: verify-full checks the host. So does verify-ca
    // (sqlx 0.8 with rustls 0.23; documented on `PgTlsMode::VerifyCa`), which fails closed.
    if let Some(port) = addr.strip_prefix("localhost:") {
      let by_ip = |mode| {
        PgPoolConfig::from_url(url(&format!("127.0.0.1:{port}")))
          .with_tls_mode(mode)
          .with_root_cert_pem(pki.ca_pem.clone())
      };
      for mode in [PgTlsMode::VerifyFull, PgTlsMode::VerifyCa] {
        assert_refused(
          session_ssl(by_ip(mode)).await,
          "certificate not valid for name",
        );
      }
    }
    let verify_ca = PgPoolConfig::from_url(url(&addr))
      .with_tls_mode(PgTlsMode::VerifyCa)
      .with_root_cert_pem(pki.ca_pem.clone());
    assert_eq!(session_ssl(verify_ca).await, Ok(true));

    // `prefer` (the default) and `require` encrypt without checking the certificate.
    assert_eq!(
      session_ssl(PgPoolConfig::from_url(url(&addr))).await,
      Ok(true)
    );
    let require = PgPoolConfig::from_url(url(&addr)).with_tls_mode(PgTlsMode::Require);
    assert_eq!(session_ssl(require).await, Ok(true));
    // An explicit `Disable` overrides the URL's `sslmode=require`.
    let disabled = PgPoolConfig::from_url(format!("{}?sslmode=require", url(&addr)))
      .with_tls_mode(PgTlsMode::Disable);
    assert_eq!(session_ssl(disabled).await, Ok(false));
    let _ = std::fs::remove_file(ca_file);
  }

  #[tokio::test]
  async fn pg_require_refuses_a_server_without_tls() {
    let t = "pg_require_refuses_a_server_without_tls";
    let (_container, addr) = match start(None).await {
      Ok(started) => started,
      Err(e) => return unavailable(t, &format!("the PostgreSQL container did not start: {e}")),
    };
    let mode = |mode| PgPoolConfig::from_url(url(&addr)).with_tls_mode(mode);
    assert_eq!(
      wait_until_ready(|| session_ssl(mode(PgTlsMode::Disable))).await,
      Ok(false)
    );
    assert_refused(
      session_ssl(mode(PgTlsMode::Require)).await,
      "server does not support TLS",
    );
    assert_refused(
      session_ssl(mode(PgTlsMode::VerifyFull)).await,
      "server does not support TLS",
    );
    let from_url = PgPoolConfig::from_url(format!("{}?sslmode=require", url(&addr)));
    assert_refused(session_ssl(from_url).await, "server does not support TLS");
    // `prefer` falls back to plaintext.
    assert_eq!(session_ssl(mode(PgTlsMode::Prefer)).await, Ok(false));
  }
}

#[cfg(feature = "mysql")]
mod mysql {
  use id_effect_sql::SqlClient;
  use id_effect_sql::mysql::{
    MySqlPoolConfig, MySqlSqlClient, MySqlTlsMode, mysql_pool_from_config,
  };
  use testcontainers::core::{IntoContainerPort, Mount, WaitFor};
  use testcontainers::runners::AsyncRunner;
  use testcontainers::{ContainerAsync, GenericImage, ImageExt};

  use super::*;

  /// MySQL 8.4 on a tmpfs data directory. With `pki`: TLS with those certificates, and
  /// `require_secure_transport` so plaintext logins are refused; without: TLS off
  /// (`--tls-version=` empty).
  async fn start(pki: Option<&Pki>) -> Result<(ContainerAsync<GenericImage>, String), String> {
    let mut args = vec![
      "--skip-log-bin",
      "--performance-schema=OFF",
      "--mysqlx=OFF",
      "--innodb-buffer-pool-size=32M",
    ];
    let mut request = GenericImage::new("mysql", "8.4")
      .with_exposed_port(3306.tcp())
      .with_wait_for(WaitFor::message_on_stderr(
        "port: 3306  MySQL Community Server",
      ))
      .with_env_var("MYSQL_ROOT_PASSWORD", PASSWORD)
      .with_mount(Mount::tmpfs_mount("/var/lib/mysql"))
      .with_startup_timeout(Duration::from_secs(180));
    match pki {
      Some(pki) => {
        request = request
          .with_copy_to("/certs/ca.pem", pki.ca_pem.clone().into_bytes())
          .with_copy_to(
            "/certs/server-cert.pem",
            pki.server_cert_pem.clone().into_bytes(),
          )
          .with_copy_to(
            "/certs/server-key.pem",
            pki.server_key_pem.clone().into_bytes(),
          );
        args.extend([
          "--ssl-ca=/certs/ca.pem",
          "--ssl-cert=/certs/server-cert.pem",
          "--ssl-key=/certs/server-key.pem",
          "--require-secure-transport=ON",
        ]);
      }
      None => args.push("--tls-version="),
    }
    let container = request
      .with_cmd(args)
      .start()
      .await
      .map_err(|e| e.to_string())?;
    let host = container.get_host().await.map_err(|e| e.to_string())?;
    let port = container
      .get_host_port_ipv4(3306)
      .await
      .map_err(|e| e.to_string())?;
    Ok((container, format!("{host}:{port}")))
  }

  fn url(addr: &str) -> String {
    format!("mysql://root:{PASSWORD}@{addr}/mysql")
  }

  /// The session's TLS cipher (`Ssl_cipher`, empty on a plaintext connection).
  async fn session_cipher(config: MySqlPoolConfig) -> Result<String, SqlError> {
    let pool = mysql_pool_from_config(config.with_max_size(1))
      .await
      .map_err(|e| e.0)?;
    let rows =
      run(MySqlSqlClient::new(pool.clone()).query("SHOW SESSION STATUS LIKE 'Ssl_cipher'", &[]))
        .await?;
    pool.close().await;
    match rows[0].values() {
      [_, SqlValue::Text(cipher)] => Ok(cipher.clone()),
      other => panic!("Ssl_cipher: {other:?}"),
    }
  }

  #[tokio::test]
  async fn mysql_verify_identity_with_the_right_ca_is_encrypted() {
    let t = "mysql_verify_identity_with_the_right_ca_is_encrypted";
    let pki = pki();
    let (_container, addr) = match start(Some(&pki)).await {
      Ok(started) => started,
      Err(e) => return unavailable(t, &format!("the MySQL container did not start: {e}")),
    };
    let verified = || {
      MySqlPoolConfig::from_url(url(&addr))
        .with_tls_mode(MySqlTlsMode::VerifyIdentity)
        .with_root_cert_pem(pki.ca_pem.clone())
    };
    let cipher = wait_until_ready(|| session_cipher(verified()))
      .await
      .expect("VERIFY_IDENTITY connects");
    assert!(!cipher.is_empty(), "no TLS cipher");

    // The URL form: `ssl-mode` and `ssl-ca` (a file) are honoured.
    let ca_file = pem_file(t, &pki.ca_pem);
    let from_url = format!(
      "{}?ssl-mode=VERIFY_IDENTITY&ssl-ca={}",
      url(&addr),
      ca_file.display()
    );
    let config = MySqlPoolConfig::from_url(&from_url);
    assert_eq!(
      config.effective_tls_mode().unwrap(),
      MySqlTlsMode::VerifyIdentity
    );
    assert!(!session_cipher(config).await.expect("URL form").is_empty());
    let config = MySqlPoolConfig::from_url(url(&addr))
      .with_tls_mode(MySqlTlsMode::VerifyIdentity)
      .with_root_cert_file(&ca_file);
    assert!(!session_cipher(config).await.expect("CA file").is_empty());

    // A CA that did not issue the server certificate, or none: no connection.
    let wrong_ca = MySqlPoolConfig::from_url(url(&addr))
      .with_tls_mode(MySqlTlsMode::VerifyIdentity)
      .with_root_cert_pem(pki.other_ca_pem.clone());
    assert_refused(session_cipher(wrong_ca).await, "invalid peer certificate");
    let no_ca = MySqlPoolConfig::from_url(url(&addr)).with_tls_mode(MySqlTlsMode::VerifyIdentity);
    assert_refused(session_cipher(no_ca).await, "invalid peer certificate");
    let overridden =
      MySqlPoolConfig::from_url(&from_url).with_root_cert_pem(pki.other_ca_pem.clone());
    assert_refused(session_cipher(overridden).await, "invalid peer certificate");

    // The certificate names `localhost` only: VERIFY_IDENTITY checks the host. So does
    // VERIFY_CA (sqlx 0.8 with rustls 0.23; documented on `MySqlTlsMode::VerifyCa`).
    if let Some(port) = addr.strip_prefix("localhost:") {
      let by_ip = |mode| {
        MySqlPoolConfig::from_url(url(&format!("127.0.0.1:{port}")))
          .with_tls_mode(mode)
          .with_root_cert_pem(pki.ca_pem.clone())
      };
      for mode in [MySqlTlsMode::VerifyIdentity, MySqlTlsMode::VerifyCa] {
        assert_refused(
          session_cipher(by_ip(mode)).await,
          "certificate not valid for name",
        );
      }
    }
    let verify_ca = MySqlPoolConfig::from_url(url(&addr))
      .with_tls_mode(MySqlTlsMode::VerifyCa)
      .with_root_cert_pem(pki.ca_pem.clone());
    assert!(
      !session_cipher(verify_ca)
        .await
        .expect("VERIFY_CA")
        .is_empty()
    );

    // PREFERRED (the default) encrypts without checking the certificate.
    assert!(
      !session_cipher(MySqlPoolConfig::from_url(url(&addr)))
        .await
        .expect("PREFERRED")
        .is_empty()
    );
    // An explicit `Disabled` overrides the URL's `ssl-mode=REQUIRED`: the server, requiring
    // secure transport, refuses the plaintext login with its own error (SQLSTATE tagged).
    let disabled = MySqlPoolConfig::from_url(format!("{}?ssl-mode=REQUIRED", url(&addr)))
      .with_tls_mode(MySqlTlsMode::Disabled);
    assert_refused(session_cipher(disabled).await, "(SQLSTATE HY000)");
    let _ = std::fs::remove_file(ca_file);
  }

  #[tokio::test]
  async fn mysql_required_refuses_a_server_without_tls() {
    let t = "mysql_required_refuses_a_server_without_tls";
    let (_container, addr) = match start(None).await {
      Ok(started) => started,
      Err(e) => return unavailable(t, &format!("the MySQL container did not start: {e}")),
    };
    let mode = |mode| MySqlPoolConfig::from_url(url(&addr)).with_tls_mode(mode);
    let cipher = wait_until_ready(|| session_cipher(mode(MySqlTlsMode::Disabled)))
      .await
      .expect("DISABLED connects");
    assert_eq!(cipher, "");
    assert_refused(
      session_cipher(mode(MySqlTlsMode::Required)).await,
      "server does not support TLS",
    );
    assert_refused(
      session_cipher(mode(MySqlTlsMode::VerifyIdentity)).await,
      "server does not support TLS",
    );
    let from_url = MySqlPoolConfig::from_url(format!("{}?ssl-mode=REQUIRED", url(&addr)));
    assert_refused(
      session_cipher(from_url).await,
      "server does not support TLS",
    );
    // PREFERRED falls back to plaintext.
    assert_eq!(
      session_cipher(mode(MySqlTlsMode::Preferred)).await,
      Ok(String::new())
    );
  }
}
