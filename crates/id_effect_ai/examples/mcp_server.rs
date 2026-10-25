//! An MCP server exposing a small document store: tools `search_documents` and `get_document`
//! (the same `Tool` values an agent loop would run), a resource and a prompt.
//!
//! ```text
//! # stdio (what desktop MCP hosts launch):
//! cargo run -p id_effect_ai --features mcp,mcp-http --example mcp_server
//! # Streamable HTTP at http://127.0.0.1:8787/mcp, bearer token from MCP_TOKEN (default demo-token):
//! cargo run -p id_effect_ai --features mcp,mcp-http --example mcp_server -- --http 127.0.0.1:8787
//! # start both in-process, call them with McpClient, exit 0 when every check passes:
//! cargo run -p id_effect_ai --features mcp,mcp-http --example mcp_server -- --self-test
//! ```
//!
//! The HTTP endpoint is guarded (an MCP endpoint without an access decision does not compile),
//! refuses browser origins and caps messages at 1 MiB. The bearer check below stands in for a
//! real authentication layer (`id_effect_auth`).

use std::collections::BTreeMap;
use std::net::SocketAddr;

use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;
use id_effect::{Effect, Exit, Schema, fail, succeed};
use id_effect_ai::mcp::{
  McpClient, McpEndpoint, McpError, McpServer, Prompt, PromptArgument, PromptMessage, Resource,
  ResourceBody,
};
use id_effect_ai::tools::{Tool, ToolSet};
use id_effect_authz::{BasicPrincipal, CurrentPrincipal, Guard, require_role};
use id_effect_platform::http::HttpClientConfig;
use id_effect_tokio::TokioRuntime;
use serde_json::json;

const DOCS: [(i64, &str, &str); 3] = [
  (
    1,
    "Invoice 2026-001",
    "Acme Corp owes 1200 EUR, due 2026-11-30.",
  ),
  (
    2,
    "Contract renewal",
    "The support contract renews on 2027-01-01.",
  ),
  (3, "Invoice 2026-002", "Globex owes 300 EUR, paid."),
];

#[derive(Clone, Debug, Schema)]
struct Query {
  query: String,
}

#[derive(Clone, Debug, Schema)]
struct Hit {
  id: i64,
  title: String,
}

#[derive(Clone, Debug, Schema)]
struct Hits {
  hits: Vec<Hit>,
}

#[derive(Clone, Debug, Schema)]
struct DocId {
  id: i64,
}

#[derive(Clone, Debug, Schema)]
struct Document {
  id: i64,
  title: String,
  body: String,
}

fn server() -> McpServer {
  let search = Tool::new(
    "search_documents",
    "Finds documents whose title or body contains the query (case-insensitive)",
    |q: Query| -> Effect<Hits, String> {
      let needle = q.query.to_lowercase();
      let hits = DOCS
        .iter()
        .filter(|(_, t, b)| {
          t.to_lowercase().contains(&needle) || b.to_lowercase().contains(&needle)
        })
        .map(|(id, title, _)| Hit {
          id: *id,
          title: (*title).to_owned(),
        })
        .collect();
      succeed(Hits { hits })
    },
  );
  let get = Tool::new(
    "get_document",
    "Returns one document by id",
    |d: DocId| -> Effect<Document, String> {
      match DOCS.iter().find(|(id, _, _)| *id == d.id) {
        Some((id, title, body)) => succeed(Document {
          id: *id,
          title: (*title).to_owned(),
          body: (*body).to_owned(),
        }),
        None => fail(format!("no document {}", d.id)),
      }
    },
  );
  let tools = ToolSet::new()
    .with(search)
    .and_then(|t| t.with(get))
    .expect("valid tools");
  McpServer::new("document-store", env!("CARGO_PKG_VERSION"))
    .with_instructions("Search documents, then fetch the ones you need by id.")
    .with_tools(tools)
    .with_resource(
      Resource::new("docs://index", "index")
        .with_mime_type("text/plain")
        .with_description("Every document title"),
      || {
        let index: Vec<String> = DOCS.iter().map(|(id, t, _)| format!("{id}: {t}")).collect();
        succeed::<_, String, _>(ResourceBody::Text(index.join("\n")))
      },
    )
    .with_prompt(
      Prompt::new("summarize")
        .with_description("Summarize one document")
        .with_argument(PromptArgument::required("id", "the document id")),
      |args: BTreeMap<String, String>| {
        succeed::<_, String, _>(vec![PromptMessage::user(format!(
          "Call get_document with id {} and summarize it in one sentence.",
          args.get("id").map_or("?", String::as_str)
        ))])
      },
    )
}

/// The stand-in authentication layer: a request with the right bearer token acts as principal
/// `demo` with role `agent`; anything else stays anonymous (and the guard answers 401).
async fn authenticate(token: String, mut req: Request, next: Next) -> Response {
  let presented = req
    .headers()
    .get("authorization")
    .and_then(|v| v.to_str().ok())
    .and_then(|v| v.strip_prefix("Bearer "));
  if presented == Some(token.as_str()) {
    req.extensions_mut().insert(CurrentPrincipal::new(
      BasicPrincipal::new("demo").with_role("agent"),
    ));
  }
  next.run(req).await
}

fn http_app(token: String) -> axum::Router {
  McpEndpoint::new(server())
    .guard(Guard::new("mcp.agent", require_role("agent")))
    .router::<()>()
    .into_axum()
    .layer(axum::middleware::from_fn(move |req, next| {
      authenticate(token.clone(), req, next)
    }))
}

async fn run<A: Send + 'static>(eff: Effect<A, McpError>) -> Result<A, String> {
  match TokioRuntime::current().expect("tokio").run(eff).await {
    Exit::Success(a) => Ok(a),
    Exit::Failure(cause) => Err(format!("{cause:?}")),
  }
}

/// Exercises one client against the document store.
async fn check(client: &McpClient, transport: &str) -> Result<(), String> {
  let desc = run(client.discover()).await?;
  let tools = run(client.list_tools()).await?;
  let names: Vec<_> = tools.iter().map(|t| t.name.as_str()).collect();
  if names != ["get_document", "search_documents"] {
    return Err(format!("{transport}: unexpected tools {names:?}"));
  }
  let found = run(client.call_tool("search_documents", json!({ "query": "invoice" }))).await?;
  let hits = found
    .structured_content
    .as_ref()
    .and_then(|v| v["hits"].as_array().cloned())
    .unwrap_or_default();
  if hits.len() != 2 {
    return Err(format!("{transport}: expected 2 invoices, got {hits:?}"));
  }
  let doc = run(client.call_tool("get_document", json!({ "id": 2 }))).await?;
  if doc.structured_content.as_ref().map(|v| v["title"].clone()) != Some(json!("Contract renewal"))
  {
    return Err(format!("{transport}: wrong document {doc:?}"));
  }
  let missing = run(client.call_tool("get_document", json!({ "id": 99 }))).await?;
  if !missing.is_error {
    return Err(format!(
      "{transport}: a missing document must be a tool error"
    ));
  }
  let index = run(client.read_resource("docs://index")).await?;
  let prompt = run(client.get_prompt(
    "summarize",
    BTreeMap::from([("id".to_owned(), "1".to_owned())]),
  ))
  .await?;
  println!(
    "{transport}: {} v{} ok — {} tools, index of {} lines, prompt of {} message(s)",
    desc.server_info.map(|i| i.name).unwrap_or_default(),
    env!("CARGO_PKG_VERSION"),
    tools.len(),
    index[0].text().unwrap_or_default().lines().count(),
    prompt.messages.len(),
  );
  Ok(())
}

async fn self_test() -> Result<(), String> {
  // stdio over an in-memory pipe.
  let (client_io, server_io) = tokio::io::duplex(1 << 20);
  let (sr, sw) = tokio::io::split(server_io);
  tokio::spawn(
    TokioRuntime::current()
      .expect("tokio")
      .run(server().serve_io(sr, sw)),
  );
  let (cr, cw) = tokio::io::split(client_io);
  let client = run(McpClient::over_io(cr, cw)).await?;
  check(&client, "stdio").await?;

  // Streamable HTTP on a loopback port.
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
    .await
    .map_err(|e| e.to_string())?;
  let addr = listener.local_addr().map_err(|e| e.to_string())?;
  tokio::spawn(async move { axum::serve(listener, http_app("self-test".into())).await });
  let url = format!("http://{addr}/mcp");
  // The platform client refuses loopback unless the host is allowed explicitly (SSRF policy).
  let local = HttpClientConfig::default().allow_hosts(["127.0.0.1"]);
  let client = McpClient::http(&url, &local)
    .with_bearer_token("self-test")
    .map_err(|e| e.to_string())?;
  check(&client, "http").await?;
  let anonymous = McpClient::http(&url, &local);
  match run(anonymous.discover()).await {
    Err(e) if e.contains("401") => {}
    other => return Err(format!("an anonymous caller must get 401, got {other:?}")),
  }
  let blocked = McpClient::http(&url, &HttpClientConfig::default());
  match run(blocked.discover()).await {
    Err(e) if e.contains("Blocked") => {}
    other => {
      return Err(format!(
        "the default SSRF policy must refuse loopback, got {other:?}"
      ));
    }
  }
  Ok(())
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
  let args: Vec<String> = std::env::args().skip(1).collect();
  let outcome = match args.first().map(String::as_str) {
    Some("--self-test") => self_test().await,
    Some("--http") => {
      let addr: SocketAddr = args
        .get(1)
        .map_or("127.0.0.1:8787", String::as_str)
        .parse()
        .expect("--http takes host:port");
      let token = std::env::var("MCP_TOKEN").unwrap_or_else(|_| "demo-token".into());
      let listener = tokio::net::TcpListener::bind(addr).await.expect("bind");
      eprintln!("MCP Streamable HTTP at http://{addr}/mcp (bearer token from MCP_TOKEN)");
      axum::serve(listener, http_app(token))
        .await
        .map_err(|e| e.to_string())
    }
    Some(other) => Err(format!(
      "unknown argument {other}; use --http [addr] or --self-test"
    )),
    // Logs go to stderr: stdout carries the protocol.
    None => run(server().serve_stdio()).await,
  };
  match outcome {
    Ok(()) => {
      if args.first().map(String::as_str) == Some("--self-test") {
        println!("mcp_server self-test: ok");
      }
      std::process::ExitCode::SUCCESS
    }
    Err(e) => {
      eprintln!("mcp_server: {e}");
      std::process::ExitCode::FAILURE
    }
  }
}
