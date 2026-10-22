//! Interop with `rmcp` (D7: a dev-dependency only, exact pin; ADR 0015 §7), in both directions
//! and on both transports, on the pinned revision 2026-07-28.
#![cfg(all(feature = "mcp-http", feature = "test-util"))]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use id_effect::{Effect, Exit, Schema, succeed};
use id_effect_ai::mcp::{
  McpClient, McpEndpoint, McpError, McpServer, PROTOCOL_VERSION, Prompt, PromptArgument,
  PromptMessage, Resource, ResourceBody, report_progress,
};
use id_effect_ai::tools::{Tool, ToolSet};
use id_effect_authz::{BasicPrincipal, CurrentPrincipal, Guard, require_role};
use id_effect_platform::http::HttpClientConfig;
use id_effect_tokio::TokioRuntime;
use rmcp::model::{
  CallToolRequestParams, CallToolResponse, CallToolResult, ClientRequest, GetPromptRequestParams,
  GetPromptResponse, GetPromptResult, ListPromptsResult, ListResourcesResult, ListToolsResult,
  PaginatedRequestParams, ProtocolVersion, ReadResourceRequestParams, ReadResourceResponse,
  ReadResourceResult, ServerConfig,
};
use rmcp::service::{ClientLifecycleMode, ClientServiceExt, RequestContext, RoleClient};
use rmcp::{ErrorData, RoleServer, ServerHandler, ServiceExt};
use serde_json::{Value, json};

#[derive(Clone, Debug, Schema)]
struct Add {
  a: i64,
  b: i64,
}

#[derive(Clone, Debug, Schema)]
struct Sum {
  sum: i64,
}

fn ours() -> McpServer {
  let tools = ToolSet::new()
    .with(Tool::new(
      "add",
      "Adds two integers",
      |i: Add| -> Effect<Sum, String> {
        report_progress(1.0, Some(1.0), None).flat_map(move |()| succeed(Sum { sum: i.a + i.b }))
      },
    ))
    .unwrap();
  McpServer::new("id-effect-docs", "1.0.0")
    .with_instructions("Documents and arithmetic.")
    .with_tools(tools)
    .with_resource(
      Resource::new("doc://readme", "readme").with_mime_type("text/plain"),
      || succeed::<_, String, _>(ResourceBody::Text("hello from id_effect".into())),
    )
    .with_prompt(
      Prompt::new("summarize").with_argument(PromptArgument::required("doc", "the document")),
      |args: BTreeMap<String, String>| {
        succeed::<_, String, _>(vec![PromptMessage::user(format!(
          "Summarize {}",
          args["doc"]
        ))])
      },
    )
}

fn modern() -> ClientLifecycleMode {
  ClientLifecycleMode::Discover {
    preferred_versions: vec![ProtocolVersion::V_2026_07_28],
  }
}

fn de<T: serde::de::DeserializeOwned>(v: Value) -> T {
  serde_json::from_value(v).expect("an rmcp value")
}

/// Drives an rmcp client through every capability of [`ours`].
async fn exercise_rmcp_client(client: &rmcp::service::RunningService<RoleClient, ()>) {
  let info = client.peer_info().expect("discovered");
  assert_eq!(info.server_info.as_ref().unwrap().name, "id-effect-docs");
  assert_eq!(info.protocol_version, ProtocolVersion::V_2026_07_28);

  let tools = client.list_all_tools().await.unwrap();
  assert_eq!(tools.len(), 1);
  assert_eq!(tools[0].name, "add");
  assert_eq!(tools[0].input_schema.get("type"), Some(&json!("object")));

  let result = client
    .call_tool(de::<CallToolRequestParams>(
      json!({ "name": "add", "arguments": { "a": 20, "b": 22 } }),
    ))
    .await
    .unwrap();
  assert_eq!(result.structured_content, Some(json!({ "sum": 42 })));
  assert_ne!(result.is_error, Some(true));
  let bad = client
    .call_tool(de::<CallToolRequestParams>(
      json!({ "name": "add", "arguments": { "a": "x" } }),
    ))
    .await
    .unwrap();
  assert_eq!(bad.is_error, Some(true));
  assert!(
    client
      .call_tool(de::<CallToolRequestParams>(json!({ "name": "nope" })))
      .await
      .is_err()
  );

  let resources = client.list_resources(None).await.unwrap();
  assert_eq!(resources.resources[0].uri, "doc://readme");
  let read = client
    .read_resource(de::<ReadResourceRequestParams>(
      json!({ "uri": "doc://readme" }),
    ))
    .await
    .unwrap();
  let text = serde_json::to_value(&read.contents[0]).unwrap();
  assert_eq!(text["text"], "hello from id_effect");

  let prompts = client.list_prompts(None).await.unwrap();
  assert_eq!(prompts.prompts[0].name, "summarize");
  let prompt = client
    .get_prompt(de::<GetPromptRequestParams>(
      json!({ "name": "summarize", "arguments": { "doc": "7" } }),
    ))
    .await
    .unwrap();
  let message = serde_json::to_value(&prompt.messages[0]).unwrap();
  assert_eq!(message["content"]["text"], "Summarize 7");
}

#[tokio::test]
async fn mcp_interop_rmcp_client_against_our_server() {
  // stdio: rmcp's line transport over an in-memory pipe.
  let (client_io, server_io) = tokio::io::duplex(1 << 20);
  let (sr, sw) = tokio::io::split(server_io);
  tokio::spawn(
    TokioRuntime::current()
      .unwrap()
      .run(ours().serve_io(sr, sw)),
  );
  let client =
    ().serve_with_lifecycle(tokio::io::split(client_io), modern())
      .await
      .expect("rmcp discovers our server");
  exercise_rmcp_client(&client).await;
  client.cancel().await.unwrap();

  // Streamable HTTP: rmcp's reqwest client against our guarded endpoint.
  let app = McpEndpoint::new(ours())
    .guard(Guard::new("mcp", require_role("agent")))
    .router::<()>()
    .into_axum()
    .layer(axum::middleware::from_fn(
      |mut req: axum::extract::Request, next: axum::middleware::Next| async move {
        if req.headers().get("authorization").is_some() {
          req.extensions_mut().insert(CurrentPrincipal::new(
            BasicPrincipal::new("rmcp").with_role("agent"),
          ));
        }
        next.run(req).await
      },
    ));
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let addr = listener.local_addr().unwrap();
  tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
  let transport = rmcp::transport::StreamableHttpClientTransport::from_config(
    rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig::with_uri(
      format!("http://{addr}/mcp"),
    )
    .auth_header("token"),
  );
  let client =
    ().serve_with_lifecycle(transport, modern())
      .await
      .expect("rmcp discovers our HTTP endpoint");
  exercise_rmcp_client(&client).await;
  client.cancel().await.unwrap();
}

/// An `rmcp` server with one tool, one resource and one prompt, built from wire JSON.
#[derive(Clone)]
struct RmcpPeer;

impl ServerHandler for RmcpPeer {
  fn get_info(&self) -> ServerConfig {
    de(json!({
      "protocolVersion": PROTOCOL_VERSION,
      "capabilities": { "tools": {}, "resources": {}, "prompts": {} },
      "serverInfo": { "name": "rmcp-peer", "version": "3.4.0" },
      "instructions": "an rmcp server",
    }))
  }

  async fn list_tools(
    &self,
    _request: Option<PaginatedRequestParams>,
    _context: RequestContext<RoleServer>,
  ) -> Result<ListToolsResult, ErrorData> {
    Ok(de(json!({ "tools": [{
      "name": "shout",
      "description": "Upper-cases text",
      "inputSchema": { "type": "object", "properties": {
        "text": { "type": "string" },
        "region": { "type": "string", "x-mcp-header": "Region" },
      }, "required": ["text"] },
    }] })))
  }

  async fn call_tool(
    &self,
    request: CallToolRequestParams,
    _context: RequestContext<RoleServer>,
  ) -> Result<CallToolResponse, ErrorData> {
    if request.name != "shout" {
      return Err(ErrorData::invalid_params("unknown tool", None));
    }
    let args = request.arguments.unwrap_or_default();
    let text = args.get("text").and_then(Value::as_str).unwrap_or_default();
    let result: CallToolResult = de(json!({
      "content": [{ "type": "text", "text": text.to_uppercase() }],
      "structuredContent": { "shouted": text.to_uppercase() },
      "isError": false,
    }));
    Ok(result.into())
  }

  async fn list_resources(
    &self,
    _request: Option<PaginatedRequestParams>,
    _context: RequestContext<RoleServer>,
  ) -> Result<ListResourcesResult, ErrorData> {
    Ok(de(json!({ "resources": [
      { "uri": "mem://note", "name": "note", "mimeType": "text/plain" },
    ] })))
  }

  async fn read_resource(
    &self,
    request: ReadResourceRequestParams,
    _context: RequestContext<RoleServer>,
  ) -> Result<ReadResourceResponse, ErrorData> {
    let result: ReadResourceResult = de(json!({ "contents": [
      { "uri": request.uri, "mimeType": "text/plain", "text": "from rmcp" },
    ] }));
    Ok(result.into())
  }

  async fn list_prompts(
    &self,
    _request: Option<PaginatedRequestParams>,
    _context: RequestContext<RoleServer>,
  ) -> Result<ListPromptsResult, ErrorData> {
    Ok(de(json!({ "prompts": [
      { "name": "greet", "arguments": [{ "name": "who", "required": true }] },
    ] })))
  }

  async fn get_prompt(
    &self,
    request: GetPromptRequestParams,
    _context: RequestContext<RoleServer>,
  ) -> Result<GetPromptResponse, ErrorData> {
    let who = request
      .arguments
      .and_then(|a| a.get("who").and_then(Value::as_str).map(str::to_owned))
      .unwrap_or_default();
    let result: GetPromptResult = de(json!({ "messages": [
      { "role": "user", "content": { "type": "text", "text": format!("Hello {who}") } },
    ] }));
    Ok(result.into())
  }
}

async fn run<A: Send + 'static>(eff: Effect<A, McpError>) -> A {
  match TokioRuntime::current().unwrap().run(eff).await {
    Exit::Success(a) => a,
    Exit::Failure(c) => panic!("{c:?}"),
  }
}

/// Drives our client through every capability of [`RmcpPeer`].
async fn exercise_our_client(client: &McpClient) {
  let desc = run(client.discover()).await;
  assert!(
    desc
      .supported_versions
      .contains(&PROTOCOL_VERSION.to_owned())
  );
  assert!(desc.has_capability("tools"));
  assert_eq!(desc.server_info.unwrap().name, "rmcp-peer");

  let tools = run(client.list_tools()).await;
  assert_eq!(tools[0].name, "shout");
  // The `x-mcp-header` argument is mirrored into `Mcp-Param-Region` (rmcp checks it over HTTP).
  let out = run(client.call_tool("shout", json!({ "text": "hi", "region": "eu-wést" }))).await;
  assert!(!out.is_error);
  assert_eq!(out.text(), "HI");
  assert_eq!(out.structured_content, Some(json!({ "shouted": "HI" })));
  assert!(
    TokioRuntime::current()
      .unwrap()
      .run(client.call_tool("nope", json!({})))
      .await
      .into_result()
      .is_err()
  );

  let resources = run(client.list_resources()).await;
  assert_eq!(resources[0].uri, "mem://note");
  let contents = run(client.read_resource("mem://note")).await;
  assert_eq!(contents[0].text(), Some("from rmcp"));

  let prompts = run(client.list_prompts()).await;
  assert!(prompts[0].arguments[0].required);
  let args = BTreeMap::from([("who".to_owned(), "id_effect".to_owned())]);
  let reply = run(client.get_prompt("greet", args)).await;
  assert_eq!(reply.messages[0].content.as_text(), Some("Hello id_effect"));
}

#[tokio::test]
async fn mcp_interop_our_client_against_rmcp_server() {
  // stdio.
  let (client_io, server_io) = tokio::io::duplex(1 << 20);
  tokio::spawn(async move {
    let running = RmcpPeer
      .serve(tokio::io::split(server_io))
      .await
      .expect("rmcp serves");
    let _ = running.waiting().await;
  });
  let (cr, cw) = tokio::io::split(client_io);
  let client = run(McpClient::over_io(cr, cw)).await;
  exercise_our_client(&client).await;

  // Streamable HTTP: rmcp's tower service on loopback, reached through the platform client.
  use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
  use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService,
  };
  let service = StreamableHttpService::new(
    || Ok(RmcpPeer),
    Arc::new(LocalSessionManager::default()),
    StreamableHttpServerConfig::default(),
  );
  let app = axum::Router::new().nest_service("/mcp", service);
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let addr = listener.local_addr().unwrap();
  tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
  let client = McpClient::http(
    format!("http://{addr}/mcp"),
    &HttpClientConfig::default().allow_hosts(["127.0.0.1"]),
  )
  .with_timeout(Duration::from_secs(20));
  exercise_our_client(&client).await;
}

/// Keeps the import of `ClientRequest` meaningful: the rmcp request our client's first call
/// becomes is a `server/discover`.
#[test]
fn our_discover_is_rmcp_discover() {
  let wire = json!({ "method": "server/discover", "params": { "_meta": {
    "io.modelcontextprotocol/protocolVersion": PROTOCOL_VERSION,
    "io.modelcontextprotocol/clientCapabilities": {},
  } } });
  let req: ClientRequest = de(wire);
  assert_eq!(req.method(), "server/discover");
}
