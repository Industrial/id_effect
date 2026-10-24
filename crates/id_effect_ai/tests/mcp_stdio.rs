//! MCP over stdio (ai.llm #6, ADR 0015 §7): tools from a `ToolSet`, resources, prompts,
//! progress and cancellation, server and client over an in-memory pipe.
#![cfg(all(feature = "mcp", feature = "test-util"))]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use id_effect::{Effect, Exit, Schema, from_async, succeed, sync};
use id_effect_ai::mcp::{
  INVALID_PARAMS, INVALID_REQUEST, McpClient, McpError, McpLimits, McpServer, PARSE_ERROR,
  PROTOCOL_VERSION, Prompt, PromptArgument, PromptMessage, PromptRole, Resource, ResourceBody,
  SERVER_BUSY, UNSUPPORTED_PROTOCOL_VERSION, report_progress,
};
use id_effect_ai::tools::{Tool, ToolSet};
use id_effect_tokio::TokioRuntime;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};
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

fn tools() -> ToolSet {
  ToolSet::new()
    .with(Tool::new(
      "add",
      "Adds two integers",
      |i: Add| -> Effect<Sum, String> { succeed(Sum { sum: i.a + i.b }) },
    ))
    .unwrap()
    .with(Tool::new(
      "steps",
      "Reports progress n times",
      |i: Steps| -> Effect<Done, String> {
        let mut eff: Effect<(), String> = succeed(());
        for k in 1..=i.n {
          eff = eff.flat_map(move |()| report_progress(k as f64, Some(i.n as f64), None));
        }
        eff.map(|()| Done { ok: true })
      },
    ))
    .unwrap()
    .with(
      Tool::new(
        "wipe",
        "Deletes everything",
        |_: Steps| -> Effect<Done, String> { succeed(Done { ok: true }) },
      )
      .writes(),
    )
    .unwrap()
}

fn server() -> McpServer {
  McpServer::new("docs", "1.0.0")
    .with_instructions("Ask about documents.")
    .with_tools(tools())
    .with_resource(
      Resource::new("doc://readme", "readme")
        .with_mime_type("text/plain")
        .with_description("What this server holds"),
      || succeed::<_, String, _>(ResourceBody::Text("two documents".into())),
    )
    .with_resource(Resource::new("doc://logo", "logo"), || {
      succeed::<_, String, _>(ResourceBody::Blob(vec![0x89, b'P', b'N', b'G']))
    })
    .with_resource(Resource::new("doc://broken", "broken"), || {
      id_effect::fail::<ResourceBody, String, _>("disk on fire".into())
    })
    .with_prompt(
      Prompt::new("summarize")
        .with_description("Summarize a document")
        .with_argument(PromptArgument::required("doc", "the document id"))
        .with_argument(PromptArgument::optional("style", "the tone")),
      |args: BTreeMap<String, String>| {
        let style = args.get("style").cloned().unwrap_or_else(|| "plain".into());
        succeed::<_, String, _>(vec![
          PromptMessage::user(format!("Summarize {} ({style})", args["doc"])),
          PromptMessage::assistant("Sure."),
        ])
      },
    )
}

fn pipe(server: McpServer) -> (DuplexStream, tokio::task::JoinHandle<Exit<(), McpError>>) {
  let (client_io, server_io) = tokio::io::duplex(1 << 20);
  let (r, w) = tokio::io::split(server_io);
  let serving = tokio::spawn(TokioRuntime::current().unwrap().run(server.serve_io(r, w)));
  (client_io, serving)
}

async fn connect(server: McpServer) -> McpClient {
  let (io, _serving) = pipe(server);
  let (r, w) = tokio::io::split(io);
  ok(McpClient::over_io(r, w)).await
}

#[tokio::test]
async fn mcp_tools_round_trip_stdio() {
  let client = connect(server()).await;
  let desc = ok(client.discover()).await;
  assert_eq!(desc.supported_versions, vec![PROTOCOL_VERSION.to_owned()]);
  assert!(desc.has_capability("tools"));
  assert_eq!(desc.server_info.unwrap().name, "docs");
  assert_eq!(desc.instructions.as_deref(), Some("Ask about documents."));

  let listed = ok(client.list_tools()).await;
  let names: Vec<_> = listed.iter().map(|t| t.name.as_str()).collect();
  // The write tool is neither listed nor callable without the opt-out.
  assert_eq!(names, ["add", "steps"]);
  assert!(listed.iter().all(|t| t.read_only_hint()));
  assert_eq!(listed[0].input_schema["type"], "object");

  let sum = ok(client.call_tool("add", json!({ "a": 2, "b": 40 }))).await;
  assert!(!sum.is_error);
  assert_eq!(sum.structured_content, Some(json!({ "sum": 42 })));
  assert_eq!(sum.text(), r#"{"sum":42}"#);

  // Arguments are decoded through the tool's Schema: a bad one is a tool error the model reads.
  let bad = ok(client.call_tool("add", json!({ "a": "two" }))).await;
  assert!(bad.is_error);
  assert!(bad.text().contains("invalid arguments"));
  let denied = ok(client.call_tool("wipe", json!({ "n": 1 }))).await;
  assert!(denied.is_error);

  // An unknown tool never runs: a protocol error.
  let unknown = err(client.call_tool("rm_rf", json!({}))).await;
  assert_eq!(unknown.rpc().unwrap().code, INVALID_PARAMS);

  // Progress reaches the caller.
  let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
  let s = Arc::clone(&seen);
  let done = ok(
    client.call_tool_with_progress("steps", json!({ "n": 3 }), move |p| {
      s.lock().unwrap().push((p.progress, p.total));
    }),
  )
  .await;
  assert_eq!(done.structured_content, Some(json!({ "ok": true })));
  let seen = seen.lock().unwrap().clone();
  assert_eq!(
    seen,
    vec![(1.0, Some(3.0)), (2.0, Some(3.0)), (3.0, Some(3.0))]
  );

  // Methods outside the revision are refused.
  assert!(err(client.request("ping", json!({}))).await.rpc().is_some());
}

#[tokio::test]
async fn mcp_resources_and_prompts() {
  let client = connect(server()).await;
  let resources = ok(client.list_resources()).await;
  let uris: Vec<_> = resources.iter().map(|r| r.uri.as_str()).collect();
  assert_eq!(uris, ["doc://broken", "doc://logo", "doc://readme"]);
  assert_eq!(resources[2].mime_type.as_deref(), Some("text/plain"));

  let readme = ok(client.read_resource("doc://readme")).await;
  assert_eq!(readme[0].text(), Some("two documents"));
  assert_eq!(readme[0].mime_type.as_deref(), Some("text/plain"));
  let logo = ok(client.read_resource("doc://logo")).await;
  assert_eq!(
    logo[0].body,
    ResourceBody::Blob(vec![0x89, b'P', b'N', b'G'])
  );
  let missing = err(client.read_resource("doc://nope")).await;
  assert_eq!(missing.rpc().unwrap().code, INVALID_PARAMS);
  let broken = err(client.read_resource("doc://broken")).await;
  assert!(broken.rpc().unwrap().message.contains("disk on fire"));

  let prompts = ok(client.list_prompts()).await;
  assert_eq!(prompts[0].name, "summarize");
  assert_eq!(prompts[0].arguments.len(), 2);
  let args = BTreeMap::from([("doc".to_owned(), "42".to_owned())]);
  let reply = ok(client.get_prompt("summarize", args)).await;
  assert_eq!(reply.description.as_deref(), Some("Summarize a document"));
  assert_eq!(reply.messages.len(), 2);
  assert_eq!(reply.messages[0].role, PromptRole::User);
  assert_eq!(
    reply.messages[0].content.as_text(),
    Some("Summarize 42 (plain)")
  );
  let missing_arg = err(client.get_prompt("summarize", BTreeMap::new())).await;
  assert!(missing_arg.rpc().unwrap().message.contains("doc"));
  let unknown = err(client.get_prompt("nope", BTreeMap::new())).await;
  assert_eq!(unknown.rpc().unwrap().code, INVALID_PARAMS);
  let not_string = err(client.request(
    "prompts/get",
    json!({ "name": "summarize", "arguments": { "doc": 1 } }),
  ))
  .await;
  assert_eq!(not_string.rpc().unwrap().code, INVALID_PARAMS);
}

#[tokio::test]
async fn mcp_cancellation_interrupts_tool_fiber() {
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
  let server = McpServer::new("s", "1").with_tools(ToolSet::new().with(hang).unwrap());
  let client = connect(server).await;
  let call = tokio::spawn(
    TokioRuntime::current()
      .unwrap()
      .run(client.call_tool("hang", json!({ "n": 1 }))),
  );
  tokio::time::timeout(Duration::from_secs(10), started.notified())
    .await
    .expect("the tool started");
  // Dropping the call sends `notifications/cancelled`; the server interrupts the tool's fiber.
  call.abort();
  let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
  while !finalized.load(Ordering::SeqCst) {
    assert!(
      tokio::time::Instant::now() < deadline,
      "the finalizer never ran"
    );
    tokio::time::sleep(Duration::from_millis(10)).await;
  }
  // The connection still serves.
  assert!(ok(client.discover()).await.has_capability("tools"));
}

/// Writes raw lines and reads raw answers.
struct Raw {
  write: tokio::io::WriteHalf<DuplexStream>,
  read: BufReader<tokio::io::ReadHalf<DuplexStream>>,
}

impl Raw {
  async fn send(&mut self, line: &str) {
    self.write.write_all(line.as_bytes()).await.unwrap();
    self.write.write_all(b"\n").await.unwrap();
  }

  async fn recv(&mut self) -> Value {
    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(10), self.read.read_line(&mut line))
      .await
      .expect("an answer")
      .unwrap();
    serde_json::from_str(&line).unwrap()
  }
}

fn meta() -> Value {
  json!({
    "io.modelcontextprotocol/protocolVersion": PROTOCOL_VERSION,
    "io.modelcontextprotocol/clientCapabilities": {},
  })
}

#[tokio::test]
async fn stdio_refuses_malformed_excess_and_unversioned_input() {
  let started = Arc::new(AtomicUsize::new(0));
  let release = Arc::new(Notify::new());
  let (s, r) = (Arc::clone(&started), Arc::clone(&release));
  let slow = Tool::new("slow", "Waits", move |_: Steps| -> Effect<Done, String> {
    let s = Arc::clone(&s);
    let r = Arc::clone(&r);
    from_async(move || async move {
      s.fetch_add(1, Ordering::SeqCst);
      r.notified().await;
      Ok(Done { ok: true })
    })
  });
  let server = McpServer::new("s", "1")
    .with_tools(ToolSet::new().with(slow).unwrap())
    .with_limits(
      McpLimits::default()
        .with_max_message_bytes(512)
        .with_max_in_flight(1),
    );
  let (io, serving) = pipe(server);
  let (read, write) = tokio::io::split(io);
  let mut raw = Raw {
    write,
    read: BufReader::new(read),
  };

  raw.send(&"x".repeat(2048)).await;
  let too_long = raw.recv().await;
  assert_eq!(too_long["error"]["code"], INVALID_REQUEST);
  assert_eq!(too_long["id"], Value::Null);
  raw.send("not json").await;
  assert_eq!(raw.recv().await["error"]["code"], PARSE_ERROR);
  raw.send("   ").await;
  raw
    .send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
    .await;
  raw.send(r#"{"jsonrpc":"2.0","id":5,"result":{}}"#).await;
  raw
    .send(r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":99}}"#)
    .await;
  raw
    .send(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#)
    .await;
  let unversioned = raw.recv().await;
  assert_eq!(unversioned["id"], 1);
  assert_eq!(unversioned["error"]["code"], INVALID_PARAMS);
  let old = json!({ "jsonrpc": "2.0", "id": 2, "method": "server/discover", "params": {
    "_meta": { "io.modelcontextprotocol/protocolVersion": "2025-06-18",
               "io.modelcontextprotocol/clientCapabilities": {} } } });
  raw.send(&old.to_string()).await;
  let refused = raw.recv().await;
  assert_eq!(refused["error"]["code"], UNSUPPORTED_PROTOCOL_VERSION);
  assert_eq!(
    refused["error"]["data"]["supported"],
    json!([PROTOCOL_VERSION])
  );

  // One request in flight at most: a second is refused, a duplicate id too.
  let call = |id: i64| {
    json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": { "name": "slow", "arguments": { "n": 1 }, "_meta": meta() } })
    .to_string()
  };
  raw.send(&call(10)).await;
  while started.load(Ordering::SeqCst) == 0 {
    tokio::time::sleep(Duration::from_millis(5)).await;
  }
  raw.send(&call(10)).await;
  let duplicate = raw.recv().await;
  assert_eq!(duplicate["error"]["code"], INVALID_REQUEST);
  raw.send(&call(11)).await;
  let busy = raw.recv().await;
  assert_eq!(busy["id"], 11);
  assert_eq!(busy["error"]["code"], SERVER_BUSY);
  release.notify_one();
  let answered = raw.recv().await;
  assert_eq!(answered["id"], 10);
  assert_eq!(
    answered["result"]["structuredContent"],
    json!({ "ok": true })
  );

  // End of input: the server finishes and stops.
  drop(raw);
  let exit = tokio::time::timeout(Duration::from_secs(10), serving)
    .await
    .unwrap()
    .unwrap();
  assert!(matches!(exit, Exit::Success(())));
}

#[tokio::test]
async fn dropping_every_client_ends_the_servers_input() {
  let (io, serving) = pipe(server());
  let (r, w) = tokio::io::split(io);
  let client = ok(McpClient::over_io(r, w)).await;
  assert!(ok(client.discover()).await.has_capability("prompts"));
  drop(client);
  let exit = tokio::time::timeout(Duration::from_secs(10), serving)
    .await
    .expect("the server stops at end of input")
    .unwrap();
  assert!(matches!(exit, Exit::Success(())));
}

#[tokio::test]
async fn the_client_fails_waiting_calls_when_the_server_goes_away() {
  let (client_io, server_io) = tokio::io::duplex(1 << 16);
  let (cr, cw) = tokio::io::split(client_io);
  let client = ok(McpClient::over_io(cr, cw)).await;
  let (sr, mut sw) = tokio::io::split(server_io);
  let mut sr = BufReader::new(sr);
  let pending = tokio::spawn(TokioRuntime::current().unwrap().run(client.discover()));
  let mut line = String::new();
  sr.read_line(&mut line).await.unwrap();
  let req: Value = serde_json::from_str(&line).unwrap();
  assert_eq!(req["method"], "server/discover");
  assert_eq!(
    req["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"],
    PROTOCOL_VERSION
  );
  assert_eq!(
    req["params"]["_meta"]["io.modelcontextprotocol/clientInfo"]["name"],
    "id_effect_ai"
  );
  // A server request is refused (the client offers no capabilities); noise is ignored.
  sw.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":\"s1\",\"method\":\"sampling/createMessage\"}\n")
    .await
    .unwrap();
  sw.write_all(b"garbage\n{\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\",\"params\":{\"progressToken\":77,\"progress\":1}}\n")
    .await
    .unwrap();
  sw.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":42,\"result\":{}}\n{\"jsonrpc\":\"2.0\",\"error\":{\"code\":1,\"message\":\"x\"}}\n")
    .await
    .unwrap();
  sw.write_all(&[b'x'; 2 << 20]).await.unwrap();
  sw.write_all(b"\n").await.unwrap();
  line.clear();
  sr.read_line(&mut line).await.unwrap();
  let refusal: Value = serde_json::from_str(&line).unwrap();
  assert_eq!(refusal["id"], "s1");
  assert_eq!(refusal["error"]["code"], -32601);
  drop(sw);
  drop(sr);
  let exit = pending.await.unwrap();
  let Exit::Failure(cause) = exit else {
    panic!("the call cannot succeed");
  };
  assert!(matches!(cause.fail_option(), Some(McpError::Transport(_))));
  // Later calls fail at once.
  assert!(matches!(
    err(client.discover()).await,
    McpError::Transport(_)
  ));
  assert!(format!("{client:?}").contains("stdio"));
  // HTTP-only settings leave a stdio client as it is.
  let same = client
    .clone()
    .with_timeout(Duration::from_secs(1))
    .with_header("x-api-key", "k")
    .unwrap()
    .with_max_message_bytes(1024)
    .with_client_info(id_effect_ai::mcp::Implementation::new("me", "1"));
  assert!(format!("{same:?}").contains("\"me\""));
}
