//! MCP over Streamable HTTP (ADR 0015 §7, B16): the guarded `id_effect_axum` route, the Origin
//! allowlist, the message cap, principal-bound handles, and the client under the platform SSRF
//! policy.
#![cfg(all(feature = "mcp-http", feature = "test-util"))]

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use axum::extract::Request;
use axum::http::request::Parts;
use axum::middleware::Next;
use axum::response::Response;
use id_effect::observability::posture::dangerous_opt_outs;
use id_effect::{Effect, Exit, Schema, fail, from_async, succeed, sync};
use id_effect_ai::mcp::{
  DANGEROUS_MCP_WRITE_TOOLS, HEADER_MISMATCH, INVALID_PARAMS, McpClient, McpEndpoint, McpError,
  McpLimits, McpServer, PROTOCOL_VERSION, Prompt, PromptMessage, Resource, ResourceBody,
  SERVER_BUSY, UNSUPPORTED_PROTOCOL_VERSION, mint_handle, report_progress, resolve_handle,
};
use id_effect_ai::tools::{Tool, ToolSet};
use id_effect_authz::{BasicPrincipal, CurrentPrincipal, Guard, require_role};
use id_effect_axum::realtime::origin::OriginPolicy;
use id_effect_axum::route::{GuardFuture, RouteGuard};
use id_effect_platform::http::{
  HttpClient, HttpClientConfig, HttpRequest, HttpResponse, ReqwestHttpClient,
};
use id_effect_tokio::TokioRuntime;
use serde_json::{Value, json};
use tokio::sync::Notify;

#[derive(Clone, Debug, Schema)]
struct Add {
  a: i64,
  b: i64,
}

#[derive(Clone, Debug, Schema)]
struct Sum {
  sum: i64,
}

#[derive(Clone, Debug, Schema)]
struct Cart {
  item: String,
}

#[derive(Clone, Debug, Schema)]
struct Opened {
  handle: String,
}

#[derive(Clone, Debug, Schema)]
struct HandleArg {
  handle: String,
}

#[derive(Clone, Debug, Schema)]
struct Steps {
  n: i64,
}

#[derive(Clone, Debug, Schema)]
struct Done {
  ok: bool,
}

async fn run<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> Exit<A, E> {
  TokioRuntime::current().expect("tokio").run(eff).await
}

async fn ok<A: Send + 'static>(eff: Effect<A, McpError>) -> A {
  match run(eff).await {
    Exit::Success(a) => a,
    Exit::Failure(cause) => panic!("call failed: {cause:?}"),
  }
}

async fn err<A: Send + 'static>(eff: Effect<A, McpError>) -> McpError {
  match run(eff).await {
    Exit::Success(_) => panic!("expected a failure"),
    Exit::Failure(cause) => cause.fail_option().expect("a typed failure"),
  }
}

/// Counts calls of tool `add`.
static ADDS: AtomicUsize = AtomicUsize::new(0);

fn tools() -> ToolSet {
  ToolSet::new()
    .with(Tool::new("add", "Adds", |i: Add| -> Effect<Sum, String> {
      ADDS.fetch_add(1, Ordering::SeqCst);
      succeed(Sum { sum: i.a + i.b })
    }))
    .unwrap()
    .with(Tool::new(
      "open_cart",
      "Opens a cart; returns its handle",
      |c: Cart| -> Effect<Opened, String> {
        mint_handle(json!({ "item": c.item }))
          .map(|handle| Opened { handle })
          .map_err(|e| e.to_string())
      },
    ))
    .unwrap()
    .with(Tool::new(
      "read_cart",
      "Reads a cart by handle",
      |h: HandleArg| -> Effect<Cart, String> {
        resolve_handle(h.handle)
          .map(|v| Cart {
            item: v["item"].as_str().unwrap_or_default().to_owned(),
          })
          .map_err(|e| e.to_string())
      },
    ))
    .unwrap()
    .with(Tool::new(
      "steps",
      "Reports progress",
      |i: Steps| -> Effect<Done, String> {
        let mut eff: Effect<(), String> = succeed(());
        for k in 1..=i.n {
          eff = eff.flat_map(move |()| report_progress(k as f64, None, Some(format!("step {k}"))));
        }
        eff.map(|()| Done { ok: true })
      },
    ))
    .unwrap()
    .with(
      Tool::new("wipe", "Deletes", |_: Steps| -> Effect<Done, String> {
        succeed(Done { ok: true })
      })
      .writes(),
    )
    .unwrap()
}

fn server() -> McpServer {
  McpServer::new("docs", "1.0.0")
    .with_tools(tools())
    .with_resource(Resource::new("doc://readme", "readme"), || {
      succeed::<_, String, _>(ResourceBody::Text("hello".into()))
    })
    .with_prompt(Prompt::new("greet"), |_| {
      succeed::<_, String, _>(vec![PromptMessage::user("hi")])
    })
}

/// Turns `Authorization: Bearer <user>` into an authenticated principal with role `agent`
/// (the test's stand-in for an authentication layer); counts every request.
async fn auth(counter: Arc<AtomicUsize>, mut req: Request, next: Next) -> Response {
  counter.fetch_add(1, Ordering::SeqCst);
  let user = req
    .headers()
    .get("authorization")
    .and_then(|v| v.to_str().ok())
    .and_then(|v| v.strip_prefix("Bearer "))
    .map(str::to_owned);
  if let Some(user) = user {
    let role = if user == "intern" { "visitor" } else { "agent" };
    req.extensions_mut().insert(CurrentPrincipal::new(
      BasicPrincipal::new(user).with_role(role),
    ));
  }
  next.run(req).await
}

struct Served {
  addr: SocketAddr,
  hits: Arc<AtomicUsize>,
}

impl Served {
  fn url(&self) -> String {
    format!("http://{}/mcp", self.addr)
  }

  fn client(&self, user: &str) -> McpClient {
    McpClient::http(self.url(), &local())
      .with_bearer_token(user)
      .unwrap()
      .with_timeout(Duration::from_secs(20))
  }
}

async fn serve_router(router: axum::Router) -> Served {
  let hits = Arc::new(AtomicUsize::new(0));
  let h = Arc::clone(&hits);
  let app = router.layer(axum::middleware::from_fn(move |req, next| {
    auth(Arc::clone(&h), req, next)
  }));
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let addr = listener.local_addr().unwrap();
  tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
  Served { addr, hits }
}

async fn serve(endpoint: McpEndpoint<id_effect_ai::mcp::Guarded>) -> Served {
  serve_router(endpoint.router::<()>().into_axum()).await
}

fn guarded(server: McpServer) -> McpEndpoint<id_effect_ai::mcp::Guarded> {
  McpEndpoint::new(server).guard(Guard::new("mcp.agent", require_role("agent")))
}

/// The platform client, allowed to reach the loopback test server.
fn local() -> HttpClientConfig {
  HttpClientConfig::default().allow_hosts(["127.0.0.1"])
}

/// A raw POST with `headers` (no MCP client involved).
async fn post(url: &str, body: Vec<u8>, headers: &[(&str, &str)]) -> HttpResponse {
  let client = ReqwestHttpClient::from_config(&local());
  let mut req = HttpRequest::post(url, body);
  req.headers = headers
    .iter()
    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
    .collect();
  match run(client.execute(req)).await {
    Exit::Success(r) => r,
    Exit::Failure(c) => panic!("http failed: {c:?}"),
  }
}

fn discover_body(id: i64) -> Vec<u8> {
  json!({ "jsonrpc": "2.0", "id": id, "method": "server/discover", "params": { "_meta": {
    "io.modelcontextprotocol/protocolVersion": PROTOCOL_VERSION,
    "io.modelcontextprotocol/clientCapabilities": {},
  } } })
  .to_string()
  .into_bytes()
}

const BASE: [(&str, &str); 4] = [
  ("content-type", "application/json"),
  ("authorization", "Bearer alice"),
  ("mcp-protocol-version", PROTOCOL_VERSION),
  ("mcp-method", "server/discover"),
];

fn body_json(r: &HttpResponse) -> Value {
  serde_json::from_slice(&r.body).unwrap()
}

#[tokio::test]
async fn mcp_streamable_http_round_trip() {
  let served = serve(guarded(server())).await;
  let client = served.client("alice");
  let desc = ok(client.discover()).await;
  assert_eq!(desc.supported_versions, vec![PROTOCOL_VERSION.to_owned()]);
  assert!(desc.has_capability("resources") && desc.has_capability("prompts"));

  let tools = ok(client.list_tools()).await;
  let names: Vec<_> = tools.iter().map(|t| t.name.as_str()).collect();
  assert_eq!(names, ["add", "open_cart", "read_cart", "steps"]);
  let sum = ok(client.call_tool("add", json!({ "a": 1, "b": 2 }))).await;
  assert_eq!(sum.structured_content, Some(json!({ "sum": 3 })));
  let unknown = err(client.call_tool("nope", json!({}))).await;
  assert_eq!(unknown.rpc().unwrap().code, INVALID_PARAMS);

  // A progress token turns the answer into an event stream: progress, then the result.
  let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
  let s = Arc::clone(&seen);
  let done = ok(
    client.call_tool_with_progress("steps", json!({ "n": 2 }), move |p| {
      s.lock().unwrap().push(p.message.unwrap_or_default());
    }),
  )
  .await;
  assert_eq!(done.structured_content, Some(json!({ "ok": true })));
  assert_eq!(*seen.lock().unwrap(), ["step 1", "step 2"]);

  let contents = ok(client.read_resource("doc://readme")).await;
  assert_eq!(contents[0].text(), Some("hello"));
  assert_eq!(ok(client.list_resources()).await.len(), 1);
  assert_eq!(ok(client.list_prompts()).await[0].name, "greet");
  let reply = ok(client.get_prompt("greet", BTreeMap::new())).await;
  assert_eq!(reply.messages[0].content.as_text(), Some("hi"));

  // Only POST is served; notifications are accepted without an answer.
  let note =
    json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": { "requestId": 1 } });
  let r = post(
    &served.url(),
    note.to_string().into_bytes(),
    &[
      ("content-type", "application/json"),
      ("authorization", "Bearer alice"),
      ("mcp-protocol-version", PROTOCOL_VERSION),
      ("mcp-method", "notifications/cancelled"),
    ],
  )
  .await;
  assert_eq!(r.status, 202);
  let get = ReqwestHttpClient::from_config(&local());
  let status = |authorization: Option<&str>| {
    let mut req = HttpRequest::get(served.url());
    if let Some(a) = authorization {
      req.headers.push(("authorization".into(), a.into()));
    }
    let eff = get.execute(req);
    async move {
      match run(eff).await {
        Exit::Success(r) => r.status,
        Exit::Failure(c) => panic!("{c:?}"),
      }
    }
  };
  // Only `POST` is a declared route: the router's deny-by-default layer answers anything else
  // with 404, for every caller.
  assert_eq!(status(None).await, 404);
  assert_eq!(status(Some("Bearer alice")).await, 404);
}

#[tokio::test]
async fn r9_mcp_foreign_origin_refused() {
  let served = serve(guarded(server())).await;
  let mut headers = BASE.to_vec();
  headers.push(("origin", "https://evil.example"));
  let refused = post(&served.url(), discover_body(1), &headers).await;
  assert_eq!(refused.status, 403);
  // Without Origin (a non-browser client) the request is served.
  let served_ok = post(&served.url(), discover_body(1), &BASE).await;
  assert_eq!(served_ok.status, 200);

  // An allowlisted origin is served; others still are not.
  let allowed = serve(
    guarded(server()).allow_origins(OriginPolicy::allow(["https://app.example.com"]).unwrap()),
  )
  .await;
  let mut good = BASE.to_vec();
  good.push(("origin", "https://app.example.com"));
  assert_eq!(
    post(&allowed.url(), discover_body(1), &good).await.status,
    200
  );
  assert_eq!(
    post(&allowed.url(), discover_body(1), &headers)
      .await
      .status,
    403
  );
}

#[tokio::test]
async fn mcp_session_bound_to_principal() {
  let served = serve(guarded(
    server().with_limits(McpLimits::default().with_page_size(1)),
  ))
  .await;
  let alice = served.client("alice");
  let bob = served.client("bob");

  // A handle alice's call minted is 128-bit random and works for her only.
  let opened = ok(alice.call_tool("open_cart", json!({ "item": "book" }))).await;
  let handle = opened.structured_content.unwrap()["handle"]
    .as_str()
    .unwrap()
    .to_owned();
  assert_eq!(handle.len(), 32);
  assert!(handle.bytes().all(|b| b.is_ascii_hexdigit()));
  let mine = ok(alice.call_tool("read_cart", json!({ "handle": handle }))).await;
  assert_eq!(mine.structured_content, Some(json!({ "item": "book" })));
  let stolen = ok(bob.call_tool("read_cart", json!({ "handle": handle }))).await;
  assert!(stolen.is_error);
  assert!(stolen.text().contains("unknown handle"));

  // So is a list cursor.
  let first = ok(alice.request("tools/list", json!({}))).await;
  let cursor = first["nextCursor"].as_str().unwrap().to_owned();
  let next = ok(alice.request("tools/list", json!({ "cursor": cursor }))).await;
  assert_eq!(next["tools"].as_array().unwrap().len(), 1);
  let refused = err(bob.request("tools/list", json!({ "cursor": cursor }))).await;
  assert_eq!(refused.rpc().unwrap().code, INVALID_PARAMS);
  // Following every page works for each principal.
  assert_eq!(ok(bob.list_tools()).await.len(), 4);

  // A principal the guard denies, or no principal, never reaches the server.
  let intern = served.client("intern");
  assert!(matches!(
    err(intern.discover()).await,
    McpError::Http { status: 403, .. }
  ));
  let anonymous = McpClient::http(served.url(), &local());
  assert!(matches!(
    err(anonymous.discover()).await,
    McpError::Http { status: 401, .. }
  ));
}

/// Admits every request, anonymous ones included (a misconfigured guard).
struct LetEveryoneIn;

impl RouteGuard for LetEveryoneIn {
  fn policy(&self) -> &str {
    "everyone"
  }

  fn check<'a>(&'a self, _parts: &'a Parts) -> GuardFuture<'a> {
    Box::pin(async { Ok(()) })
  }
}

#[tokio::test]
async fn an_anonymous_principal_is_refused_even_behind_a_permissive_guard() {
  let endpoint = McpEndpoint::new(server()).guard(LetEveryoneIn).at("/tools");
  assert_eq!(endpoint.path(), "/tools");
  assert_eq!(
    endpoint.guards().policies().collect::<Vec<_>>(),
    ["everyone"]
  );
  assert!(format!("{endpoint:?}").contains("everyone"));
  let served = serve_router(endpoint.router::<()>().into_axum()).await;
  let url = format!("http://{}/tools", served.addr);
  let anonymous = McpClient::http(&url, &local());
  assert!(matches!(
    err(anonymous.discover()).await,
    McpError::Http { status: 401, .. }
  ));
  let alice = McpClient::http(&url, &local())
    .with_bearer_token("alice")
    .unwrap();
  assert!(ok(alice.discover()).await.has_capability("tools"));
}

#[tokio::test]
async fn r4_mcp_message_over_cap_rejected() {
  let served = serve(guarded(server())).await;
  let before = ADDS.load(Ordering::SeqCst);
  let big = "x".repeat(1024 * 1024);
  let body = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
    "name": "add", "arguments": { "a": 1, "b": 2, "pad": big },
    "_meta": { "io.modelcontextprotocol/protocolVersion": PROTOCOL_VERSION,
               "io.modelcontextprotocol/clientCapabilities": {} } } });
  let headers = [
    ("content-type", "application/json"),
    ("authorization", "Bearer alice"),
    ("mcp-protocol-version", PROTOCOL_VERSION),
    ("mcp-method", "tools/call"),
    ("mcp-name", "add"),
  ];
  let r = post(&served.url(), body.to_string().into_bytes(), &headers).await;
  assert_eq!(r.status, 413);
  assert_eq!(body_json(&r)["error"]["code"], -32600);
  assert_eq!(ADDS.load(Ordering::SeqCst), before, "the tool never ran");

  // The client refuses to send one too, and a smaller server cap applies the same way.
  let client = served.client("alice");
  let e = err(client.call_tool(
    "add",
    json!({ "a": 1, "b": 2, "pad": "x".repeat(1024 * 1024) }),
  ))
  .await;
  assert!(matches!(e, McpError::TooLarge { .. }));
  let small = serve(guarded(
    server().with_limits(McpLimits::default().with_max_message_bytes(64)),
  ))
  .await;
  let e = err(small.client("alice").discover()).await;
  assert_eq!(e.rpc().unwrap().code, -32600);
}

#[tokio::test]
async fn headers_must_agree_with_the_body() {
  let served = serve(guarded(server())).await;
  let url = served.url();
  let mut wrong_method = BASE.to_vec();
  wrong_method[3] = ("mcp-method", "tools/list");
  let r = post(&url, discover_body(1), &wrong_method).await;
  assert_eq!(r.status, 400);
  assert_eq!(body_json(&r)["error"]["code"], HEADER_MISMATCH);

  let no_version: Vec<_> = BASE
    .iter()
    .filter(|(k, _)| *k != "mcp-protocol-version")
    .copied()
    .collect();
  let r = post(&url, discover_body(1), &no_version).await;
  assert_eq!(body_json(&r)["error"]["code"], HEADER_MISMATCH);

  let mut old = BASE.to_vec();
  old[2] = ("mcp-protocol-version", "2025-06-18");
  let r = post(&url, discover_body(1), &old).await;
  assert_eq!(r.status, 400);
  assert_eq!(body_json(&r)["error"]["code"], UNSUPPORTED_PROTOCOL_VERSION);

  let older_body = json!({ "jsonrpc": "2.0", "id": 1, "method": "server/discover", "params": {
    "_meta": { "io.modelcontextprotocol/protocolVersion": "2025-11-25" } } });
  let r = post(&url, older_body.to_string().into_bytes(), &BASE).await;
  assert_eq!(body_json(&r)["error"]["code"], HEADER_MISMATCH);

  let r = post(&url, b"{not json".to_vec(), &BASE).await;
  assert_eq!(r.status, 400);
  assert_eq!(body_json(&r)["error"]["code"], -32700);

  // A tools/call whose Mcp-Name names another tool is refused before it runs.
  let call = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
    "name": "add", "arguments": { "a": 1, "b": 1 },
    "_meta": { "io.modelcontextprotocol/protocolVersion": PROTOCOL_VERSION,
               "io.modelcontextprotocol/clientCapabilities": {} } } });
  let headers = [
    ("content-type", "application/json"),
    ("authorization", "Bearer alice"),
    ("mcp-protocol-version", PROTOCOL_VERSION),
    ("mcp-method", "tools/call"),
    ("mcp-name", "wipe"),
  ];
  let r = post(&url, call.to_string().into_bytes(), &headers).await;
  assert_eq!(body_json(&r)["error"]["code"], HEADER_MISMATCH);
}

#[tokio::test]
async fn mcp_client_obeys_ssrf_policy() {
  let served = serve(guarded(server())).await;
  // The default platform policy refuses loopback: nothing is sent.
  let client = McpClient::http(served.url(), &HttpClientConfig::default())
    .with_bearer_token("alice")
    .unwrap();
  assert!(matches!(err(client.discover()).await, McpError::Blocked(_)));
  assert!(matches!(
    err(client.call_tool("add", json!({ "a": 1, "b": 1 }))).await,
    McpError::Blocked(_)
  ));
  assert_eq!(served.hits.load(Ordering::SeqCst), 0);
  // Allowing the host is the platform's explicit opt-in.
  assert!(
    ok(served.client("alice").discover())
      .await
      .has_capability("tools")
  );
  assert_eq!(served.hits.load(Ordering::SeqCst), 1);

  // Caller headers can never override what the protocol owns.
  for name in [
    "content-type",
    "Accept",
    "mcp-method",
    "MCP-Protocol-Version",
    "bad name",
  ] {
    assert!(matches!(
      McpClient::http(served.url(), &local()).with_header(name, "x"),
      Err(McpError::InvalidHeader(_))
    ));
  }
  assert!(
    McpClient::http(served.url(), &local())
      .with_header("x-api-key", "line\nbreak")
      .is_err()
  );
  let keyed = McpClient::http(served.url(), &local())
    .with_header("x-api-key", "a")
    .unwrap()
    .with_header("X-Api-Key", "b")
    .unwrap();
  assert!(format!("{keyed:?}").contains("/mcp"));
}

#[tokio::test]
async fn dangerous_mcp_write_tools_is_recorded() {
  let served = serve(guarded(server().dangerous_mcp_write_tools())).await;
  assert!(
    dangerous_opt_outs()
      .iter()
      .any(|(id, reopens)| *id == DANGEROUS_MCP_WRITE_TOOLS && reopens.starts_with("R10"))
  );
  let client = served.client("alice");
  let names: Vec<_> = ok(client.list_tools())
    .await
    .into_iter()
    .map(|t| t.name)
    .collect();
  assert!(names.contains(&"wipe".to_owned()));
  let wiped = ok(client.call_tool("wipe", json!({ "n": 1 }))).await;
  assert!(!wiped.is_error);
}

#[tokio::test]
async fn in_flight_requests_are_capped_per_principal_and_disconnects_interrupt() {
  let started = Arc::new(Notify::new());
  let finalized = Arc::new(AtomicBool::new(false));
  let (s, f) = (Arc::clone(&started), Arc::clone(&finalized));
  let hang = Tool::new(
    "hang",
    "Never finishes",
    move |_: Steps| -> Effect<Done, String> {
      let s = Arc::clone(&s);
      let f = Arc::clone(&f);
      from_async(move || async move {
        s.notify_one();
        std::future::pending::<Result<Done, String>>().await
      })
      .ensuring(sync(move || f.store(true, Ordering::SeqCst)))
    },
  );
  let boom = Tool::new("boom", "Fails", |_: Steps| -> Effect<Done, String> {
    fail("no".to_owned())
  });
  let server = McpServer::new("s", "1")
    .with_tools(ToolSet::new().with(hang).unwrap().with(boom).unwrap())
    .with_limits(McpLimits::default().with_max_in_flight(1));
  let served = serve(guarded(server)).await;
  let alice = served.client("alice");
  // A progress token makes the answer a stream; closing it interrupts the tool.
  let call = tokio::spawn(
    TokioRuntime::current()
      .unwrap()
      .run(alice.call_tool_with_progress("hang", json!({ "n": 1 }), |_| {})),
  );
  tokio::time::timeout(Duration::from_secs(10), started.notified())
    .await
    .unwrap();
  // Alice's second request is over her cap; Bob has his own.
  let busy = err(alice.discover()).await;
  assert_eq!(busy.rpc().unwrap().code, SERVER_BUSY);
  let failed = ok(served.client("bob").call_tool("boom", json!({ "n": 1 }))).await;
  assert!(failed.is_error);
  call.abort();
  let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
  while !finalized.load(Ordering::SeqCst) {
    assert!(
      tokio::time::Instant::now() < deadline,
      "the finalizer never ran"
    );
    tokio::time::sleep(Duration::from_millis(10)).await;
  }
  // The slot is free again.
  let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
  loop {
    match run(alice.discover()).await {
      Exit::Success(_) => break,
      Exit::Failure(_) if tokio::time::Instant::now() < deadline => {
        tokio::time::sleep(Duration::from_millis(10)).await;
      }
      Exit::Failure(c) => panic!("{c:?}"),
    }
  }
}
