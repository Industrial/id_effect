//! Release-gate integration test (D10): the showcase over real HTTP against a real PostgreSQL.
//!
//! Each test starts the server through the crate's public API — the scoped pool
//! ([`database_layer`]), the migration under its advisory lock ([`repo::migrate`]), the routes
//! over a [`PgTodoRepo`] and the scoped server ([`server_layer`] on `127.0.0.1:0`), or the whole
//! startup path ([`run_until`] with `DATABASE_URL` in the config) — and drives it with a real
//! HTTP client (`reqwest`).
//!
//! The tests need a database at `DATABASE_URL`. When CI provides a Postgres service it also sets
//! `ID_EFFECT_REQUIRE_PG=1`, which turns a missing or unreachable database into a test
//! **failure**. Without it, they print an explicit `SKIP` line to stderr and return.
//!
//! Test data is isolated by title: every title starts with a prefix unique to the test and the
//! run (process id, clock, counter), and the assertions only look at rows with that prefix, so
//! parallel runs and the crate's `repo::pg_tests` share the `todos` table without colliding.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use http_showcase::{
  AppConfig, AppEnv, DatabaseConfig, PgTodoRepo, RequestPolicy, StartupError, TodoRepoService,
  app_env, database_layer, repo, router, run_until, telemetry,
};
use id_effect::Exit;
use id_effect::next::{Effect, Layer, caps_next, effect_next, from_async, hlist, need};
use id_effect_axum::{EffectExecutor, HostConfig, HostError, ServerHandle, server_layer};
use id_effect_config::{ConfigProvider, MapConfigProvider, Secret};
use id_effect_sql::SqlClientService;
use id_effect_sql::pg::{PgPoolConfig, PgSqlClient, pg_pool_from_config};
use reqwest::{Client, Method, header};
use serde_json::{Value, json};
use tokio::sync::oneshot;

// ── availability ────────────────────────────────────────────────────────────────────────────

fn pg_required() -> bool {
  std::env::var("ID_EFFECT_REQUIRE_PG").is_ok_and(|v| v == "1")
}

/// Fail (when `ID_EFFECT_REQUIRE_PG=1`) or loudly skip a live test that has no database.
fn unavailable<T>(test: &str, why: &str) -> Option<T> {
  assert!(
    !pg_required(),
    "{test}: ID_EFFECT_REQUIRE_PG=1 but Postgres is unavailable: {why}"
  );
  eprintln!("SKIP {test}: {why} (set DATABASE_URL, and ID_EFFECT_REQUIRE_PG=1 to require it)");
  None
}

/// `DATABASE_URL`, once a connection to it succeeded; `None` after [`unavailable`] skipped. Any
/// failure after this check is a real failure, not a skip.
async fn reachable_database(test: &str) -> Option<String> {
  let url = match std::env::var("DATABASE_URL") {
    Ok(url) if !url.trim().is_empty() => url,
    _ => return unavailable(test, "DATABASE_URL is not set"),
  };
  let pool = match pg_pool_from_config(PgPoolConfig::from_url(url.clone()).with_max_size(1)).await {
    Ok(pool) => pool,
    Err(e) => return unavailable(test, &format!("cannot build pool: {}", e.0)),
  };
  let sql = SqlClientService::new(PgSqlClient::new(pool));
  if let Exit::Failure(cause) = EffectExecutor::global().run_effect(sql.connect()).await {
    return unavailable(test, &format!("cannot connect: {cause:?}"));
  }
  Some(url)
}

/// A title prefix no other test (or earlier run) uses.
fn unique_prefix(label: &str) -> String {
  static N: AtomicU32 = AtomicU32::new(0);
  let nanos = SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map_or(0, |d| d.as_nanos());
  let n = N.fetch_add(1, Ordering::SeqCst);
  format!("it-{label}-{}-{nanos}-{n}", std::process::id())
}

// ── the server ──────────────────────────────────────────────────────────────────────────────

fn host_config(port: u16) -> HostConfig {
  HostConfig {
    bind_host: "127.0.0.1".into(),
    bind_port: port,
    shutdown_timeout: Duration::from_secs(5),
  }
}

/// Serves the instrumented [`router`] over `env` on `127.0.0.1:0` ([`server_layer`]), sends the
/// bound address on `ready`, and keeps serving until `done` fires (or is dropped).
fn serve_ephemeral(
  env: AppEnv,
  ready: oneshot::Sender<SocketAddr>,
  done: oneshot::Receiver<()>,
) -> Effect<(), HostError> {
  let host: Layer<caps_next!(HostConfig), HostError> = Layer::succeed(hlist![host_config(0)]);
  let probe: Effect<(), HostError, caps_next!(ServerHandle)> =
    need::<ServerHandle>().flat_map(move |handle| {
      let addr = handle.local_addr();
      from_async(move || async move {
        let _ = ready.send(addr);
        let _ = done.await;
        Ok(())
      })
    });
  probe.provide_layer(host.provide_to(server_layer(telemetry::instrument(router(env)))))
}

/// The showcase on a live database: inside the scoped pool, migrate (advisory lock), then serve
/// the routes over a [`PgTodoRepo`] on an ephemeral port until `done`.
fn showcase_on(
  db: &DatabaseConfig,
  ready: oneshot::Sender<SocketAddr>,
  done: oneshot::Receiver<()>,
) -> Effect<(), StartupError> {
  let policy = RequestPolicy::new(Duration::from_secs(5), 2, Duration::from_millis(10));
  let served: Effect<(), StartupError, caps_next!(SqlClientService)> = effect_next!(|_r: &caps_next!(SqlClientService)| {
    let sql = SqlClientService::clone(~SqlClientService);
    ~repo::migrate(Arc::clone(&sql.0));
    let env = app_env(TodoRepoService::new(PgTodoRepo::new(sql)), policy);
    ~serve_ephemeral(env, ready, done)
  });
  served.provide_layer(database_layer(db))
}

fn database(url: String) -> DatabaseConfig {
  DatabaseConfig {
    url: Secret::new(url),
    max_connections: 4,
  }
}

// ── the client ──────────────────────────────────────────────────────────────────────────────

struct Api {
  client: Client,
  base: String,
}

impl Api {
  fn new(addr: SocketAddr) -> Self {
    Self {
      client: Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("http client"),
      base: format!("http://{addr}"),
    }
  }

  /// Sends one request (a JSON body when `body` is given); returns the status and the JSON body
  /// (`Null` when empty, a string when not JSON).
  async fn call(&self, method: Method, path: &str, body: Option<&str>) -> (u16, Value) {
    let mut req = self.client.request(method, format!("{}{path}", self.base));
    if let Some(body) = body {
      req = req
        .header(header::CONTENT_TYPE, "application/json")
        .body(body.to_owned());
    }
    let res = req.send().await.expect("request sent");
    let status = res.status().as_u16();
    let bytes = res.bytes().await.expect("response body");
    let json = if bytes.is_empty() {
      Value::Null
    } else {
      serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
    };
    (status, json)
  }

  /// The titles of `GET /todos` that start with `prefix` (this test's rows only).
  async fn titles(&self, prefix: &str) -> Vec<String> {
    let (status, all) = self.call(Method::GET, "/todos", None).await;
    assert_eq!(status, 200, "{all}");
    let mut titles: Vec<String> = all
      .as_array()
      .expect("a todo array")
      .iter()
      .filter_map(|t| t["title"].as_str())
      .filter(|t| t.starts_with(prefix))
      .map(str::to_owned)
      .collect();
    titles.sort();
    titles
  }
}

fn error_code(body: &Value) -> &str {
  body["error"].as_str().unwrap_or("<no error code>")
}

fn id_of(todo: &Value) -> i64 {
  todo["id"].as_i64().expect("a todo id")
}

// ── tests ───────────────────────────────────────────────────────────────────────────────────

/// The `/todos` flow end to end over HTTP on PostgreSQL: CRUD, `404` / `409` / `422`, and a
/// failed batch rolled back; plus `/health`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_todos_flow_on_postgres() {
  const TEST: &str = "http_todos_flow_on_postgres";
  let Some(url) = reachable_database(TEST).await else {
    return;
  };
  let (ready_tx, ready_rx) = oneshot::channel();
  let (done_tx, done_rx) = oneshot::channel();
  let server = EffectExecutor::global().run_effect(showcase_on(&database(url), ready_tx, done_rx));

  let client = async move {
    let addr = ready_rx.await.expect("the server started");
    assert!(addr.ip().is_loopback() && addr.port() != 0, "{addr}");
    flow(&Api::new(addr), &unique_prefix("flow")).await;
    let _ = done_tx.send(());
    addr
  };
  let (exit, addr) = tokio::join!(server, client);
  assert!(matches!(exit, Exit::Success(())), "{exit:?}");
  // Released with the scope: the listener is gone.
  assert!(
    Api::new(addr)
      .client
      .get(format!("http://{addr}/health"))
      .send()
      .await
      .is_err()
  );
}

async fn flow(api: &Api, p: &str) {
  // Liveness.
  let res = api
    .client
    .get(format!("{}/health", api.base))
    .send()
    .await
    .unwrap();
  assert_eq!(res.status().as_u16(), 200);
  assert_eq!(res.text().await.unwrap(), "ok");
  assert!(api.titles(p).await.is_empty());

  // Create (the title is trimmed), read, list, update.
  let (status, milk) = api
    .call(
      Method::POST,
      "/todos",
      Some(&json!({ "title": format!("  {p}-milk  ") }).to_string()),
    )
    .await;
  assert_eq!(status, 201, "{milk}");
  assert_eq!(milk["title"], json!(format!("{p}-milk")));
  assert_eq!(milk["completed"], json!(false));
  let milk_id = id_of(&milk);
  let milk_path = format!("/todos/{milk_id}");
  assert_eq!(
    api.call(Method::GET, &milk_path, None).await,
    (200, milk.clone())
  );
  assert_eq!(api.titles(p).await, vec![format!("{p}-milk")]);
  let (status, done) = api
    .call(Method::PATCH, &milk_path, Some(r#"{"completed":true}"#))
    .await;
  assert_eq!(status, 200, "{done}");
  assert_eq!(
    done,
    json!({ "id": milk_id, "title": format!("{p}-milk"), "completed": true })
  );
  let (status, same) = api.call(Method::PATCH, &milk_path, Some("{}")).await;
  assert_eq!((status, &same), (200, &done));

  // 409: a duplicate title, on create and on rename.
  let milk_body = json!({ "title": format!("{p}-milk") }).to_string();
  let (status, dup) = api.call(Method::POST, "/todos", Some(&milk_body)).await;
  assert_eq!((status, error_code(&dup)), (409, "conflict"), "{dup}");
  let (status, eggs) = api
    .call(
      Method::POST,
      "/todos",
      Some(&json!({ "title": format!("{p}-eggs") }).to_string()),
    )
    .await;
  assert_eq!(status, 201, "{eggs}");
  let eggs_path = format!("/todos/{}", id_of(&eggs));
  let (status, dup) = api.call(Method::PATCH, &eggs_path, Some(&milk_body)).await;
  assert_eq!((status, error_code(&dup)), (409, "conflict"), "{dup}");

  // A batch: all stored together...
  let batch = json!({ "titles": [format!("{p}-bread"), format!("{p}-jam")] }).to_string();
  let (status, stored) = api.call(Method::POST, "/todos/batch", Some(&batch)).await;
  assert_eq!(status, 201, "{stored}");
  let stored = stored.as_array().expect("a todo array").clone();
  assert_eq!(stored.len(), 2);
  // ... or, when one title conflicts, none: the transaction rolls the earlier insert back.
  let batch = json!({ "titles": [format!("{p}-tea"), format!("{p}-milk")] }).to_string();
  let (status, failed) = api.call(Method::POST, "/todos/batch", Some(&batch)).await;
  assert_eq!((status, error_code(&failed)), (409, "conflict"), "{failed}");
  // A conflict inside the batch itself rolls back too.
  let batch = json!({ "titles": [format!("{p}-rice"), format!("{p}-rice")] }).to_string();
  let (status, failed) = api.call(Method::POST, "/todos/batch", Some(&batch)).await;
  assert_eq!((status, error_code(&failed)), (409, "conflict"), "{failed}");
  assert_eq!(
    api.titles(p).await,
    vec![
      format!("{p}-bread"),
      format!("{p}-eggs"),
      format!("{p}-jam"),
      format!("{p}-milk"),
    ],
    "the failed batches left no rows"
  );

  // 422: a malformed id, an invalid title, a malformed or mistyped body, an unknown field.
  let invalid: [(Method, &str, Option<&str>); 7] = [
    (Method::GET, "/todos/abc", None),
    (Method::DELETE, "/todos/1.5", None),
    (Method::POST, "/todos", Some(r#"{"title":"   "}"#)),
    (Method::POST, "/todos", Some("{not json")),
    (Method::POST, "/todos", Some(r#"{"title":"x","extra":1}"#)),
    (Method::POST, "/todos/batch", Some(r#"{"titles":"x"}"#)),
    (Method::PATCH, &milk_path, Some(r#"{"completed":"yes"}"#)),
  ];
  for (method, path, body) in invalid {
    let (status, err) = api.call(method.clone(), path, body).await;
    assert_eq!(
      (status, error_code(&err)),
      (422, "invalid"),
      "{method} {path} {body:?}: {err}"
    );
  }
  // A body without a JSON content type is 422 too.
  let res = api
    .client
    .post(format!("{}/todos", api.base))
    .body(milk_body.clone())
    .send()
    .await
    .unwrap();
  assert_eq!(res.status().as_u16(), 422);

  // Delete, then 404 on every id route.
  assert_eq!(
    api.call(Method::DELETE, &milk_path, None).await,
    (204, Value::Null)
  );
  let gone: [(Method, Option<&str>); 3] = [
    (Method::GET, None),
    (Method::DELETE, None),
    (Method::PATCH, Some(r#"{"completed":false}"#)),
  ];
  for (method, body) in gone {
    let (status, err) = api.call(method.clone(), &milk_path, body).await;
    assert_eq!(
      (status, error_code(&err)),
      (404, "not_found"),
      "{method}: {err}"
    );
  }

  // Clean up this test's rows.
  let mut rest = vec![eggs];
  rest.extend(stored);
  for todo in rest {
    let path = format!("/todos/{}", id_of(&todo));
    assert_eq!(api.call(Method::DELETE, &path, None).await.0, 204);
  }
  assert!(api.titles(p).await.is_empty());
}

/// The production startup path: `DATABASE_URL` in the loaded [`AppConfig`] makes [`run_until`]
/// open the pool, migrate and serve the `/todos` routes on PostgreSQL.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_run_until_with_database_url_serves_postgres() {
  const TEST: &str = "http_run_until_with_database_url_serves_postgres";
  let Some(url) = reachable_database(TEST).await else {
    return;
  };
  // `run_until` binds the configured port and does not report it, so pick a free one.
  let port = {
    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    probe.local_addr().unwrap().port()
  };
  let port_str = port.to_string();
  let config = MapConfigProvider::from_pairs([
    ("HOST", "127.0.0.1"),
    ("PORT", port_str.as_str()),
    ("DATABASE_URL", url.as_str()),
    ("DATABASE_MAX_CONNECTIONS", "2"),
  ])
  .layer()
  .provide_to(AppConfig::layer());
  let (done_tx, done_rx) = oneshot::channel::<()>();
  let stop: Effect<(), HostError, caps_next!(HostConfig)> = from_async(move || async move {
    let _ = done_rx.await;
    Ok(())
  });
  let server = EffectExecutor::global().run_effect(run_until(stop).provide_layer(config));

  let addr = SocketAddr::from(([127, 0, 0, 1], port));
  let p = unique_prefix("startup");
  let client = async move {
    let api = Api::new(addr);
    let mut up = false;
    for _ in 0..250 {
      if let Ok(res) = api.client.get(format!("{}/health", api.base)).send().await {
        up = res.status().as_u16() == 200;
        break;
      }
      tokio::time::sleep(Duration::from_millis(20)).await;
    }
    if up {
      let body = json!({ "title": format!("{p}-one") }).to_string();
      let (status, todo) = api.call(Method::POST, "/todos", Some(&body)).await;
      assert_eq!(status, 201, "{todo}");
      let path = format!("/todos/{}", id_of(&todo));
      assert_eq!(api.call(Method::GET, &path, None).await, (200, todo));
      assert_eq!(api.titles(&p).await, vec![format!("{p}-one")]);
      assert_eq!(api.call(Method::DELETE, &path, None).await.0, 204);
    }
    let _ = done_tx.send(());
    up
  };
  let (exit, up) = tokio::join!(server, client);
  assert!(matches!(exit, Exit::Success(())), "{exit:?}");
  assert!(up, "the server never answered /health on {addr}");
}
