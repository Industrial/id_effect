//! Containers as effect resources against a real Docker daemon: each image starts, answers on its
//! published port, and is removed when its scope closes — on success, failure and interrupt.
//!
//! Without Docker every test prints `SKIP` and passes, unless `ID_EFFECT_REQUIRE_DOCKER=1` (the
//! CI `containers-integration` job), where a missing Docker daemon fails it.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use id_effect::{Cause, Effect, Exit, HCons, HNil, caps, fail, from_async, need, sleep};
use id_effect_test::containers::{MYSQL_PORT, REDIS_PORT};
use id_effect_test::{
  Container, ContainerError, ContainerImage, ContainerSpec, Keycloak, KeycloakContainer, MySql,
  MySqlContainer, Postgres, Redis, RedisContainer, Shared, Vault, VaultContainer, Wait,
  container_exists, require_docker,
};
use id_effect_tokio::TokioRuntime;

/// A test program's error: a container failure or the program's own.
#[derive(Debug, PartialEq)]
enum TestError {
  Container(ContainerError),
  Boom,
}

impl From<ContainerError> for TestError {
  fn from(e: ContainerError) -> Self {
    Self::Container(e)
  }
}

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

fn exists(id: &str) -> bool {
  match container_exists::<ContainerError, HNil>(id).block_on() {
    Exit::Success(found) => found,
    Exit::Failure(c) => panic!("inspect failed: {c:?}"),
  }
}

/// [`exists`] from inside a tokio runtime, where `block_on` is refused.
async fn exists_now(id: &str) -> bool {
  run(container_exists::<ContainerError, HNil>(id))
    .await
    .expect("inspect")
}

/// Sends `request` to `address` and returns what the server answers (until it closes the
/// connection, `limit` bytes, or one second of silence).
fn exchange(address: &str, request: &[u8], limit: usize) -> String {
  let mut stream = TcpStream::connect(address).expect("connect");
  stream
    .set_read_timeout(Some(Duration::from_secs(1)))
    .unwrap();
  stream.write_all(request).expect("write");
  let mut answer = Vec::new();
  let mut chunk = [0_u8; 1024];
  while answer.len() < limit {
    match stream.read(&mut chunk) {
      Ok(0) | Err(_) => break,
      Ok(n) => answer.extend_from_slice(&chunk[..n]),
    }
  }
  String::from_utf8_lossy(&answer).into_owned()
}

/// The capability `C`, mapped (an effect that needs only `C`).
fn using<C, A>(
  f: impl FnOnce(Arc<C>) -> A + Send + 'static,
) -> Effect<A, ContainerError, HCons<C, HNil>>
where
  C: Send + Sync + 'static,
  A: Send + 'static,
{
  need::<C>().map(f)
}

fn http_get(address: &str, path: &str) -> String {
  let request = format!("GET {path} HTTP/1.0\r\nHost: {address}\r\nConnection: close\r\n\r\n");
  exchange(address, request.as_bytes(), usize::MAX)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn redis_layer_serves_and_is_removed_after_success() {
  if !require_docker() {
    return;
  }
  let program: Effect<_, ContainerError, caps!(RedisContainer)> =
    need::<RedisContainer>().map(|redis| {
      let address = redis.container().address(REDIS_PORT).unwrap();
      let pong = exchange(&address, b"PING\r\n", 7);
      assert_eq!(address, format!("{}:{}", redis.host(), redis.port()));
      (redis.url(), redis.container().id().to_owned(), pong)
    });
  let (url, id, pong) = run(program.provide_layer(Redis::new().layer::<ContainerError>()))
    .await
    .expect("redis");
  assert!(url.starts_with("redis://127.0.0.1:"), "{url}");
  assert_eq!(pong, "+PONG\r\n");
  assert!(
    !exists_now(&id).await,
    "the container is removed when the scope closes"
  );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failing_program_still_removes_its_container() {
  if !require_docker() {
    return;
  }
  let seen = Arc::new(Mutex::new(None));
  let record = Arc::clone(&seen);
  let program: Effect<(), TestError, caps!(RedisContainer)> =
    need::<RedisContainer>().flat_map(move |redis| {
      *record.lock().unwrap() = Some(redis.container().id().to_owned());
      fail(TestError::Boom)
    });
  let failed = run(program.provide_layer(Redis::new().layer::<TestError>())).await;
  assert_eq!(failed, Err(TestError::Boom));
  let id = seen.lock().unwrap().clone().expect("the program ran");
  assert!(!exists_now(&id).await, "removed after the failure");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_interrupted_program_still_removes_its_container() {
  if !require_docker() {
    return;
  }
  let seen: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
  let record = Arc::clone(&seen);
  let forever: Effect<String, ContainerError, caps!(RedisContainer)> = need::<RedisContainer>()
    .flat_map(move |redis| {
      *record.lock().unwrap() = Some(redis.container().id().to_owned());
      sleep(Duration::from_secs(3600)).map(|()| "the body finished".to_owned())
    });
  let watch = Arc::clone(&seen);
  // Wins as soon as the container is up: `race_first` then interrupts the body and awaits it,
  // so its layer's scope has closed when the race returns.
  let started = from_async(move || async move {
    loop {
      if let Some(id) = watch.lock().unwrap().clone() {
        return Ok(id);
      }
      tokio::time::sleep(Duration::from_millis(20)).await;
    }
  });
  let raced = forever
    .provide_layer(Redis::new().layer())
    .race_first(started);
  let id = run(raced).await.expect("race");
  assert!(!exists_now(&id).await, "removed after the interrupt");
}

#[test]
fn acquire_ties_a_postgres_container_to_a_scope() {
  if !require_docker() {
    return;
  }
  let image = Postgres::new()
    .user("ledger")
    .password("s3cret")
    .database("books")
    .ready_timeout(Duration::from_secs(90));
  let program: Effect<_, ContainerError> = Effect::scoped(move |scope| {
    image.acquire::<ContainerError, HNil>(&scope).map(|pg| {
      let answered = TcpStream::connect((pg.host(), pg.port())).is_ok();
      (pg.clone(), answered)
    })
  });
  let Exit::Success((pg, answered)) = program.block_on() else {
    panic!("postgres did not start")
  };
  assert!(answered);
  assert_eq!(
    pg.url(),
    format!("postgres://ledger:s3cret@127.0.0.1:{}/books", pg.port())
  );
  assert!(pg.url_for("other").ends_with("/other"));
  assert_eq!(
    (pg.user(), pg.password(), pg.database()),
    ("ledger", "s3cret", "books")
  );
  assert_eq!(pg.container().image(), "postgres:17-alpine");
  assert!(
    !exists(pg.container().id()),
    "removed when the scope closed"
  );
}

#[test]
fn mysql_greets_over_tcp() {
  if !require_docker() {
    return;
  }
  let program = using(|mysql: Arc<MySqlContainer>| {
    let greeting = exchange(&mysql.container().address(MYSQL_PORT).unwrap(), b"", 64);
    (mysql.clone(), greeting)
  })
  .provide_layer(MySql::new().root_password("r00t").layer::<ContainerError>());
  let Exit::Success((mysql, greeting)) = program.block_on() else {
    panic!("mysql did not start")
  };
  assert!(greeting.contains("8.4"), "server greeting: {greeting:?}");
  assert_eq!(
    mysql.url(),
    format!("mysql://test:test@127.0.0.1:{}/test", mysql.port())
  );
  assert!(mysql.url_for("other").ends_with("/other"));
  assert_eq!(
    mysql.root_url(),
    format!("mysql://root:r00t@127.0.0.1:{}/test", mysql.port())
  );
  assert_eq!(
    (mysql.user(), mysql.password(), mysql.database()),
    ("test", "test", "test")
  );
  assert_eq!(mysql.host(), "127.0.0.1");
}

#[test]
fn keycloak_serves_discovery_for_an_imported_realm() {
  if !require_docker() {
    return;
  }
  let realm = r#"{"realm": "ledger", "enabled": true}"#;
  let program = using(|kc: Arc<KeycloakContainer>| {
    let address = kc.container().address(8080).unwrap();
    let discovery = http_get(&address, "/realms/ledger/.well-known/openid-configuration");
    (kc.clone(), discovery)
  })
  .provide_layer(
    Keycloak::new()
      .admin("root", "pw")
      .import_realm(realm)
      .layer::<ContainerError>(),
  );
  let Exit::Success((kc, discovery)) = program.block_on() else {
    panic!("keycloak did not start")
  };
  assert!(
    discovery.starts_with("HTTP/1.0 200") || discovery.starts_with("HTTP/1.1 200"),
    "{discovery}"
  );
  assert!(discovery.contains(&kc.issuer("ledger")), "{discovery}");
  assert_eq!(kc.base_url(), format!("http://127.0.0.1:{}", kc.port()));
  assert_eq!((kc.admin_user(), kc.admin_password()), ("root", "pw"));
}

#[test]
fn vault_dev_server_is_initialized_and_unsealed() {
  if !require_docker() {
    return;
  }
  let program = using(|vault: Arc<VaultContainer>| {
    let address = vault.addr().trim_start_matches("http://").to_owned();
    (vault.clone(), http_get(&address, "/v1/sys/health"))
  })
  .provide_layer(Vault::new().root_token("t0ken").layer::<ContainerError>());
  let Exit::Success((vault, health)) = program.block_on() else {
    panic!("vault did not start")
  };
  assert!(health.contains(r#""initialized":true"#), "{health}");
  assert!(health.contains(r#""sealed":false"#), "{health}");
  assert_eq!(vault.root_token(), "t0ken");
  assert_eq!(vault.container().image(), "hashicorp/vault:1.20");
  assert_eq!(vault.addr(), format!("http://127.0.0.1:{}", vault.port()));
}

#[test]
fn readiness_timeout_is_a_typed_error_and_removes_the_container() {
  if !require_docker() {
    return;
  }
  let spec = ContainerSpec::new("redis", "7-alpine")
    .port(REDIS_PORT)
    .wait(Wait::stdout("a line redis never prints"))
    .ready_timeout(Duration::from_secs(2));
  let Exit::Failure(Cause::Fail(error)) = spec.start::<ContainerError, HNil>().block_on() else {
    panic!("the start should time out")
  };
  let ContainerError::NotReady { image, id, timeout } = error else {
    panic!("expected NotReady, got {error}")
  };
  assert_eq!(
    (image.as_str(), timeout),
    ("redis:7-alpine", Duration::from_secs(2))
  );
  assert!(!exists(&id), "a container that is not ready is removed");
}

#[test]
fn a_container_exiting_before_ready_is_unready() {
  if !require_docker() {
    return;
  }
  let unready = |spec: ContainerSpec| match spec.start::<ContainerError, HNil>().block_on() {
    Exit::Failure(Cause::Fail(ContainerError::Unready { id, reason, .. })) => (id, reason),
    other => panic!("expected Unready, got {other:?}"),
  };
  let exits = || ContainerSpec::new("redis", "7-alpine").cmd(["sh", "-c", "exit 3"]);

  let (id, reason) = unready(exits().wait(Wait::stdout("never")));
  assert!(reason.contains("log ended"), "{reason}");
  assert!(!exists(&id));
  let (_, reason) = unready(exits().health_cmd(["true"]).wait(Wait::healthy()));
  assert!(reason.contains("exited (code 3)"), "{reason}");
  let (_, reason) = unready(ContainerSpec::new("redis", "7-alpine").wait(Wait::healthy()));
  assert!(reason.contains("no health check"), "{reason}");
}

#[test]
fn stderr_and_delay_waits_run_in_order() {
  if !require_docker() {
    return;
  }
  let spec = ContainerSpec::new("redis", "7-alpine")
    .cmd(["sh", "-c", "echo booted >&2; exec sleep 600"])
    .wait(Wait::stderr("booted"))
    .wait(Wait::delay(Duration::from_millis(1)));
  let Exit::Success(container) = spec.start::<ContainerError, HNil>().block_on() else {
    panic!("the container did not become ready")
  };
  let id = container.id().to_owned();
  assert!(matches!(
    container.remove::<ContainerError, HNil>().block_on(),
    Exit::Success(())
  ));
  assert!(!exists(&id));
}

#[test]
fn an_unknown_image_fails_to_start() {
  if !require_docker() {
    return;
  }
  let spec = ContainerSpec::new("id-effect-test/no-such-image", "0");
  let Exit::Failure(Cause::Fail(ContainerError::Start { image, .. })) =
    spec.start::<ContainerError, HNil>().block_on()
  else {
    panic!("the start should fail")
  };
  assert_eq!(image, "id-effect-test/no-such-image:0");
}

#[test]
fn a_started_container_is_removed_by_hand_or_on_drop() {
  if !require_docker() {
    return;
  }
  let spec = ContainerSpec::new("redis", "7-alpine")
    .port(REDIS_PORT)
    .label("org.id_effect.test.case", "by-hand")
    .wait(Wait::log("Ready to accept connections"));
  let Exit::Success(container) = spec.start::<ContainerError, HNil>().block_on() else {
    panic!("redis did not start")
  };
  let id = container.id().to_owned();
  assert!(exists(&id));
  assert!(format!("{container:?}").contains(&id));
  assert!(matches!(
    container.host_port(1),
    Err(ContainerError::PortNotExposed { port: 1, .. })
  ));
  let removed: Effect<bool, ContainerError> = container
    .remove()
    .flat_map({
      let c = container.clone();
      move |()| c.remove()
    })
    .flat_map({
      let c = container.clone();
      move |()| c.exists()
    });
  assert!(
    matches!(removed.block_on(), Exit::Success(false)),
    "removed, twice"
  );

  let Exit::Success(dropped) = Redis::new().start::<ContainerError, HNil>().block_on() else {
    panic!("redis did not start")
  };
  let id = dropped.container().id().to_owned();
  drop(dropped);
  assert!(!exists(&id), "the last clone's drop removes it");
}

static SHARED_REDIS: Shared<Redis> = Shared::new(Redis::new);
static NO_IMAGE: Shared<ContainerSpec> =
  Shared::new(|| ContainerSpec::new("id-effect-test/no-such-image", "0"));

#[test]
fn a_shared_container_starts_once_and_outlives_its_layers() {
  if !require_docker() {
    return;
  }
  let id_of = || {
    using(|redis: Arc<RedisContainer>| redis.container().id().to_owned())
      .provide_layer(SHARED_REDIS.layer::<ContainerError>())
  };
  let first = match id_of().block_on() {
    Exit::Success(id) => id,
    other => panic!("shared redis did not start: {other:?}"),
  };
  let Exit::Success(second) = id_of().block_on() else {
    panic!("shared redis")
  };
  assert_eq!(first, second, "one container for the whole binary");
  assert!(
    exists(&first),
    "closing a layer's scope keeps the shared container"
  );
  let Exit::Success(direct) = SHARED_REDIS.get::<ContainerError, HNil>().block_on() else {
    panic!("shared redis")
  };
  assert_eq!(direct.container().id(), first);
  assert!(format!("{SHARED_REDIS:?}").contains("started: true"));

  // A failed start is remembered: the second test asking gets the same error at once.
  let error = || match NO_IMAGE.get::<ContainerError, HNil>().block_on() {
    Exit::Failure(Cause::Fail(e)) => e,
    other => panic!("expected a start failure, got {other:?}"),
  };
  let first_error = error();
  assert!(
    matches!(first_error, ContainerError::Start { .. }),
    "{first_error}"
  );
  assert_eq!(error(), first_error);
}

/// A user-defined image: the plain [`Container`] wrapped in a newtype capability.
#[derive(Clone)]
struct Echo;

#[derive(Clone)]
struct EchoContainer(Container);

impl ContainerImage for Echo {
  type Running = EchoContainer;

  fn spec(&self) -> ContainerSpec {
    ContainerSpec::new("redis", "7-alpine")
      .port(REDIS_PORT)
      .env("ECHO", "1")
      .tmpfs("/data")
      .health_cmd(["redis-cli", "ping"])
      .wait(Wait::healthy())
  }

  fn running(&self, container: Container) -> EchoContainer {
    EchoContainer(container)
  }

  fn container(running: &EchoContainer) -> &Container {
    &running.0
  }
}

#[test]
fn a_custom_image_with_a_health_check_is_a_layer_too() {
  if !require_docker() {
    return;
  }
  let program = using(|echo: Arc<EchoContainer>| {
    let pong = exchange(&echo.0.address(REDIS_PORT).unwrap(), b"PING\r\n", 7);
    (echo.0.id().to_owned(), pong)
  })
  .provide_layer(Echo.layer::<ContainerError>());
  let Exit::Success((id, pong)) = program.block_on() else {
    panic!("the custom image did not start")
  };
  assert_eq!(pong, "+PONG\r\n");
  assert!(!exists(&id));
}
