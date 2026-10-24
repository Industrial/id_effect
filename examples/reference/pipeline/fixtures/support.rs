//! The Pipeline test fixture, shared by `tests/{core,tenancy,ai,mcp}.rs` (`#[path]`-included).
//!
//! PostgreSQL runs in this test binary's own container (`id_effect_test`, never the ambient
//! `DATABASE_URL`); each test gets two fresh databases on it: the main one (`acme` and the job
//! queue) and `globex`'s own. Without Docker a test prints `SKIP`, unless
//! `ID_EFFECT_REQUIRE_PG=1` (or `ID_EFFECT_REQUIRE_DOCKER=1`), which turns that into a failure.
//!
//! AI is deterministic (D8): [`Scripts`] routes each request to the `ScriptedModel` of the
//! document it is about (by title), so concurrent stage jobs never take each other's replies,
//! and embeddings come from `ScriptedModel`'s hash embedder.
#![allow(dead_code)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use id_effect::{Effect, HNil, Stream, fail, succeed};
use id_effect_kit::ai::embeddings::EmbedderService;
use id_effect_kit::ai::mcp::McpServer;
use id_effect_kit::ai::testing::ScriptedModel;
use id_effect_kit::ai::tools::{Tool, ToolSet};
use id_effect_kit::ai::{
  AiError, ChatRequest, ChatResponse, CompletionChunk, LanguageModel, LanguageModelService,
};
use id_effect_kit::auth::AuthPrincipal;
use id_effect_kit::axum::run_with_env;
use id_effect_kit::platform::error::HttpError;
use id_effect_kit::platform::http::{
  HttpClient, HttpClientConfig, HttpClientService, HttpRequest, HttpResponse, ReqwestHttpClient,
  StreamingHttpResponse,
};
use id_effect_kit::rpc::RpcClient;
use id_effect_kit::rpc::client::sse::SseTransport;
use id_effect_kit::sql::{
  SqlClient, SqlError, SqlRow, SqlTransaction, SqlValue, TransactionOptions,
};
use id_effect_kit::tenancy::Tenancy;
use id_effect_kit::test::{ContainerError, Postgres, Shared, docker_available};
use pipeline::mcp::McpSettings;
use pipeline::rpc::{IntakeClient, IntakeService};
use pipeline::tenancy::{TenantConfig, Tenants, TokenKeys};
use pipeline::{App, domain};
use serde_json::{Value, json};

/// The test token secret (HS256, 40 bytes).
pub const SECRET: &[u8] = b"pipeline-test-token-secret-of-40-bytes!!";

/// The issuer of the test tokens.
pub const ISSUER: &str = "https://id.pipeline.test";

/// The browser origin the MCP endpoints admit in the tests.
pub const AGENT_ORIGIN: &str = "https://agents.pipeline.test";

static PG: Shared<Postgres> = Shared::new(Postgres::new);
static NEXT_DB: AtomicU64 = AtomicU64::new(0);

/// Runs `effect` to its value; a failure is its cause, printed.
pub async fn run<A, E>(effect: Effect<A, E, HNil>) -> Result<A, String>
where
  A: Send + 'static,
  E: std::fmt::Debug + Send + 'static,
{
  run_with_env(HNil, move |_| effect)
    .await
    .into_result()
    .map_err(|cause| format!("{cause:?}"))
}

/// Runs `effect` to its value or its typed failure (a defect or interruption panics).
pub async fn run_typed<A, E>(effect: Effect<A, E, HNil>) -> Result<A, E>
where
  A: Send + 'static,
  E: std::fmt::Debug + Send + 'static,
{
  match run_with_env(HNil, move |_| effect).await.into_result() {
    Ok(a) => Ok(a),
    Err(cause) => Err(
      cause
        .fail_option()
        .unwrap_or_else(|| panic!("not a typed failure")),
    ),
  }
}

/// Whether a missing Docker fails the test instead of skipping it.
pub fn pg_required() -> bool {
  ["ID_EFFECT_REQUIRE_PG", "ID_EFFECT_REQUIRE_DOCKER"]
    .iter()
    .any(|var| std::env::var(var).is_ok_and(|v| v == "1"))
}

/// Two fresh, empty databases on this test binary's PostgreSQL container: the main one and
/// `globex`'s. `None` (SKIP) without Docker unless it is required.
pub async fn databases(test: &str) -> Option<(String, String)> {
  if let Err(e) = tokio::task::spawn_blocking(docker_available)
    .await
    .expect("docker probe")
  {
    assert!(
      !pg_required(),
      "{test}: ID_EFFECT_REQUIRE_PG=1 but Docker is unavailable: {e}"
    );
    eprintln!("SKIP {test}: {e} (set ID_EFFECT_REQUIRE_PG=1 to require it)");
    return None;
  }
  let pg = run(PG.get::<ContainerError, HNil>())
    .await
    .expect("start PostgreSQL");
  let admin = pipeline::open_pool(&pg.url(), 2).await.expect("connect");
  let mut urls = Vec::new();
  for role in ["main", "globex"] {
    let name = format!(
      "pl_{}_{}_{}",
      role,
      std::process::id(),
      NEXT_DB.fetch_add(1, Ordering::SeqCst)
    );
    let create = admin.execute(&format!("CREATE DATABASE \"{name}\""), &[]);
    run(create).await.expect("create database");
    urls.push(pg.url_for(&name));
  }
  let globex = urls.pop().expect("globex");
  let main = urls.pop().expect("main");
  Some((main, globex))
}

// ---- deterministic AI ----

/// A language model routing each request to the script of the document it is about (the
/// `title: <title>` the stages write into the user message). A request for a title without a
/// script fails.
#[derive(Clone, Default)]
pub struct Scripts {
  by_title: Arc<Mutex<HashMap<String, ScriptedModel>>>,
}

impl Scripts {
  /// The script of the document titled `title`.
  pub fn add(&self, title: &str, model: ScriptedModel) {
    self
      .by_title
      .lock()
      .expect("scripts")
      .insert(title.to_owned(), model);
  }

  /// The script of `title` (to read the requests it saw).
  pub fn get(&self, title: &str) -> ScriptedModel {
    self
      .by_title
      .lock()
      .expect("scripts")
      .get(title)
      .cloned()
      .unwrap_or_else(|| panic!("no script for {title:?}"))
  }

  fn route(&self, req: &ChatRequest) -> Result<ScriptedModel, AiError> {
    let text: String = req
      .messages
      .iter()
      .map(|m| m.content.as_str())
      .collect::<Vec<_>>()
      .join("\n");
    let scripts = self.by_title.lock().expect("scripts");
    scripts
      .iter()
      .find(|(title, _)| text.contains(&format!("title: {title}\n")))
      .map(|(_, model)| model.clone())
      .ok_or_else(|| AiError::Upstream("no script for this document".to_owned()))
  }
}

impl LanguageModel for Scripts {
  fn complete(&self, req: ChatRequest) -> Effect<ChatResponse, AiError> {
    match self.route(&req) {
      Ok(model) => model.complete(req),
      Err(e) => fail(e),
    }
  }

  fn complete_stream(&self, req: ChatRequest) -> Effect<Stream<CompletionChunk, AiError>, AiError> {
    match self.route(&req) {
      Ok(model) => model.complete_stream(req),
      Err(e) => fail(e),
    }
  }
}

/// The extraction reply of a document.
pub fn extraction(title: &str, language: &str, organization: Option<&str>) -> String {
  json!({
    "title": title,
    "summary": format!("A document about {title}."),
    "language": language,
    "organization": organization,
  })
  .to_string()
}

/// The script of a well-behaved document: its extraction, then a classification that looks at
/// the labels and answers with `labels`.
pub fn well_behaved(title: &str, language: &str, labels: &[&str]) -> ScriptedModel {
  ScriptedModel::new()
    .reply(extraction(title, language, Some("Acme Corp")))
    .tool_call("c1", "list_labels", json!({}))
    .reply(json!({ "labels": labels }).to_string())
}

// ---- the app ----

/// One Pipeline under test: two databases, the app over them, the scripts and the tokens.
pub struct Harness {
  /// The app.
  pub app: Arc<App>,
  /// The scripted model.
  pub scripts: Scripts,
  /// The token keys.
  pub keys: TokenKeys,
  /// The main database (acme and the job queue), directly.
  pub main: Arc<dyn SqlClient>,
  /// globex's own database, directly.
  pub globex: Arc<dyn SqlClient>,
  /// The id of the `inbox` source of `acme` and of `globex`.
  pub sources: HashMap<&'static str, i64>,
}

/// The tenants under test: `acme` (en) on the main database, `globex` (de) on its own.
pub fn tenants(globex_url: &str) -> Tenants {
  Tenants::new([
    TenantConfig::new("acme", "en").expect("acme"),
    TenantConfig::new("globex", "de")
      .expect("globex")
      .with_database(globex_url),
  ])
}

impl Harness {
  /// A migrated, seeded Pipeline (each tenant has an `inbox` source and three labels);
  /// `configure` adjusts the app before it is shared. `None` (SKIP) without Docker.
  pub async fn start(test: &str, configure: impl FnOnce(App) -> App) -> Option<Self> {
    let (main_url, globex_url) = databases(test).await?;
    let tenants = tenants(&globex_url);
    pipeline::migrate(&tenants, &main_url)
      .await
      .expect("migrations");
    let control = pipeline::open_pool(&main_url, 4).await.expect("main");
    let main = pipeline::open_pool(&main_url, 2).await.expect("main");
    let globex = pipeline::open_pool(&globex_url, 2).await.expect("globex");
    let scripts = Scripts::default();
    let model: LanguageModelService = Arc::new(scripts.clone());
    let embedder: EmbedderService = Arc::new(ScriptedModel::new());
    let router = tenants.router(&main_url, 4);
    let app = configure(App::new(tenants, router, control, model, embedder));
    let app = Arc::new(app);
    let mut sources = HashMap::new();
    for (tenant, labels) in [
      ("acme", ["invoice", "contract", "spam"]),
      ("globex", ["rechnung", "vertrag", "spam"]),
    ] {
      let tenancy = Tenancy::new(tenant);
      let source = domain::create_source(&app.db, &tenancy, "inbox", "imap://inbox");
      let source = run(tenancy.scope(source)).await.expect("source");
      sources.insert(tenant, source.id);
      for label in labels {
        let create = domain::create_label(&app.db, &tenancy, label, &format!("A {label}"));
        run(tenancy.scope(create)).await.expect("label");
      }
    }
    Some(Self {
      app,
      scripts,
      keys: TokenKeys::new(SECRET.to_vec(), ISSUER).expect("keys"),
      main,
      globex,
      sources,
    })
  }

  /// A bearer token of `sub` in `tenant` with `roles`.
  pub async fn token(&self, sub: &str, tenant: Option<&str>, roles: &[&str]) -> String {
    let mut principal = AuthPrincipal::new(sub).with_roles(roles.iter().copied());
    if let Some(tenant) = tenant {
      principal = principal.with_tenant(tenant);
    }
    run(self.keys.issuer(Duration::from_secs(600)).issue(&principal))
      .await
      .expect("token")
  }

  /// The app's router on a free loopback port.
  pub async fn serve(&self) -> SocketAddr {
    let mcp = McpSettings {
      allowed_origins: vec![AGENT_ORIGIN.to_owned()],
    };
    let app = pipeline::router(&self.app, &self.keys, &mcp).expect("the router builds");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
      .await
      .expect("a port");
    let addr = listener.local_addr().expect("an address");
    tokio::spawn(async move {
      let _ = axum::serve(listener, app).await;
    });
    addr
  }

  /// Runs every runnable stage job until the queue is idle.
  pub async fn drain(&self) {
    run(pipeline::stages::run_pending(&self.app))
      .await
      .expect("the stage jobs run");
  }

  /// The status of `document_id` of `tenant`, read directly.
  pub async fn status(&self, tenant: &str, document_id: i64) -> String {
    let tenancy = Tenancy::new(tenant);
    let find = domain::find_document(&self.app.db, &tenancy, document_id);
    run(tenancy.scope(find)).await.expect("document").status
  }

  /// `count(*)` of `sql` on `db`.
  pub async fn count(db: &Arc<dyn SqlClient>, sql: &str, params: Vec<SqlValue>) -> i64 {
    let rows = run(db.query(sql, &params)).await.expect("count");
    rows[0].get::<i64>(0usize).expect("a count")
  }
}

/// The platform HTTP client for loopback (the SSRF guard refuses loopback unless allowed).
pub fn loopback_http() -> HttpClientConfig {
  HttpClientConfig::default()
    .allow_hosts(["127.0.0.1"])
    .with_timeout(Duration::from_secs(20))
}

/// The platform HTTP client sending `Authorization: Bearer <token>` with every request (the SSE
/// transport opens its streams through it).
struct WithBearer {
  client: ReqwestHttpClient,
  authorization: String,
}

impl HttpClient for WithBearer {
  fn execute(&self, mut req: HttpRequest) -> Effect<HttpResponse, HttpError> {
    req
      .headers
      .push(("authorization".to_owned(), self.authorization.clone()));
    self.client.execute(req)
  }

  fn execute_stream(&self, mut req: HttpRequest) -> Effect<StreamingHttpResponse, HttpError> {
    req
      .headers
      .push(("authorization".to_owned(), self.authorization.clone()));
    self.client.execute_stream(req)
  }
}

/// The Rust RPC client of the Pipeline at `addr`, sending `token`: queries and mutations over
/// HTTP, the status subscription as server-sent events.
pub fn client(addr: SocketAddr, token: &str) -> IntakeClient<SseTransport> {
  let http = HttpClientService::new(WithBearer {
    client: ReqwestHttpClient::from_config(&loopback_http()),
    authorization: format!("Bearer {token}"),
  });
  IntakeService::client(SseTransport::new(
    RpcClient::new(format!("http://{addr}")),
    http,
  ))
}

// ---- the query-count probe ----

/// A client counting the statements it is asked to run: the N+1 probe.
#[derive(Clone)]
pub struct Probe {
  inner: Arc<dyn SqlClient>,
  queries: Arc<Mutex<Vec<String>>>,
}

impl Probe {
  /// A probe over `inner`.
  pub fn new(inner: Arc<dyn SqlClient>) -> Self {
    Self {
      inner,
      queries: Arc::default(),
    }
  }

  /// Every statement queried so far.
  pub fn queries(&self) -> Vec<String> {
    self.queries.lock().expect("probe").clone()
  }
}

impl SqlClient for Probe {
  fn connect(&self) -> Effect<(), SqlError> {
    self.inner.connect()
  }

  fn query(&self, sql: &str, params: &[SqlValue]) -> Effect<Vec<SqlRow>, SqlError> {
    self.queries.lock().expect("probe").push(sql.to_owned());
    self.inner.query(sql, params)
  }

  fn execute(&self, sql: &str, params: &[SqlValue]) -> Effect<u64, SqlError> {
    self.inner.execute(sql, params)
  }

  fn begin(&self) -> Effect<Arc<dyn SqlTransaction>, SqlError> {
    self.inner.begin()
  }

  fn begin_with(&self, options: TransactionOptions) -> Effect<Arc<dyn SqlTransaction>, SqlError> {
    self.inner.begin_with(options)
  }

  fn query_stream(&self, sql: &str, params: &[SqlValue]) -> Stream<SqlRow, SqlError> {
    self.inner.query_stream(sql, params)
  }

  fn close(&self) -> Effect<(), id_effect::Never> {
    self.inner.close()
  }
}

// ---- the fixture MCP tool server (the enrichment side) ----

/// The input of the fixture server's `lookup_organization`.
#[derive(Clone, Debug, id_effect::Schema)]
pub struct OrganizationQuery {
  /// The organization's name.
  pub name: String,
}

/// The output of the fixture server's `lookup_organization`.
#[derive(Clone, Debug, id_effect::Schema)]
pub struct OrganizationRecord {
  /// The registered name.
  pub name: String,
  /// The country.
  pub country: String,
  /// The registry id.
  pub registry_id: String,
}

/// A fixture MCP tool server: `lookup_organization` knows `Acme Corp` and nobody else.
pub fn fixture_mcp_server() -> McpServer {
  let lookup = Tool::new(
    "lookup_organization",
    "Looks an organization up in the fixture registry",
    |q: OrganizationQuery| -> Effect<OrganizationRecord, String> {
      if q.name == "Acme Corp" {
        succeed(OrganizationRecord {
          name: "Acme Corporation".to_owned(),
          country: "BE".to_owned(),
          registry_id: "BE-0123.456.789".to_owned(),
        })
      } else {
        fail(format!("{} is not registered", q.name))
      }
    },
  );
  McpServer::new("registry", "1.0.0").with_tools(ToolSet::new().with(lookup).expect("tool"))
}

/// A JSON-RPC MCP request body.
pub fn mcp_request(id: i64, method: &str, params: Value) -> Value {
  let mut params = params;
  params["_meta"] = json!({
    "io.modelcontextprotocol/protocolVersion": id_effect_kit::ai::mcp::PROTOCOL_VERSION,
    "io.modelcontextprotocol/clientInfo": { "name": "test", "version": "1" },
    "io.modelcontextprotocol/clientCapabilities": {},
  });
  json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}
