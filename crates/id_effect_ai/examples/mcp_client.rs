//! An MCP client: enriches documents with a tool from another MCP server, reports progress and
//! cancels a call that takes too long.
//!
//! ```text
//! # against an in-process fixture server (stdio over a pipe), no network:
//! cargo run -p id_effect_ai --features mcp,mcp-http --example mcp_client
//! # against a running Streamable HTTP server (e.g. the mcp_server example with --http):
//! cargo run -p id_effect_ai --features mcp,mcp-http --example mcp_client -- \
//!   --url http://127.0.0.1:8787/mcp --token demo-token
//! ```
//!
//! Over HTTP the client sends through `id_effect_platform`'s client: its SSRF policy refuses
//! private and loopback addresses unless the host is allowed explicitly, as done below for a
//! local URL.

use std::time::Duration;

use id_effect::{Effect, Exit, Schema, from_async};
use id_effect_ai::mcp::{McpClient, McpError, McpServer, report_progress};
use id_effect_ai::tools::{Tool, ToolSet};
use id_effect_platform::http::HttpClientConfig;
use id_effect_tokio::TokioRuntime;
use serde_json::json;

#[derive(Clone, Debug, Schema)]
struct Text {
  text: String,
}

#[derive(Clone, Debug, Schema)]
struct Label {
  label: String,
  confidence: f64,
}

/// The fixture tool server: a deterministic classifier and a tool that never finishes.
fn fixture_server() -> McpServer {
  let classify = Tool::new(
    "classify",
    "Labels a document as invoice, contract or other",
    |t: Text| -> Effect<Label, String> {
      let lower = t.text.to_lowercase();
      let label = if lower.contains("owes") || lower.contains("invoice") {
        "invoice"
      } else if lower.contains("contract") {
        "contract"
      } else {
        "other"
      };
      report_progress(1.0, Some(1.0), Some("classified".into())).map(move |()| Label {
        label: label.to_owned(),
        confidence: 0.9,
      })
    },
  );
  let slow = Tool::new(
    "slow",
    "Never finishes",
    |_: Text| -> Effect<Label, String> {
      from_async(std::future::pending::<Result<Label, String>>)
    },
  );
  McpServer::new("fixture-classifier", "1.0.0").with_tools(
    ToolSet::new()
      .with(classify)
      .and_then(|t| t.with(slow))
      .expect("valid tools"),
  )
}

async fn run<A: Send + 'static>(eff: Effect<A, McpError>) -> Result<A, McpError> {
  match TokioRuntime::current().expect("tokio").run(eff).await {
    Exit::Success(a) => Ok(a),
    Exit::Failure(cause) => Err(
      cause
        .fail_option()
        .unwrap_or_else(|| McpError::Transport("interrupted".into())),
    ),
  }
}

async fn enrich(client: &McpClient) -> Result<(), McpError> {
  let desc = run(client.discover()).await?;
  println!(
    "connected to {} (revisions {:?})",
    desc.server_info.map(|i| i.name).unwrap_or_default(),
    desc.supported_versions
  );
  let tools = run(client.list_tools()).await?;
  for t in &tools {
    println!(
      "  tool {}: {}",
      t.name,
      t.description.clone().unwrap_or_default()
    );
  }
  if !tools.iter().any(|t| t.name == "classify") {
    return Ok(());
  }
  for doc in [
    "Acme Corp owes 1200 EUR",
    "The support contract renews in January",
    "Team lunch on Friday",
  ] {
    let labelled = run(
      client.call_tool_with_progress("classify", json!({ "text": doc }), |p| {
        eprintln!("  progress {}/{:?} {:?}", p.progress, p.total, p.message)
      }),
    )
    .await?;
    println!("  {doc:?} -> {}", labelled.text());
  }
  // Cancellation: dropping the call (here, a timeout) cancels it on the server, which
  // interrupts the tool's fiber.
  let slow = run(client.call_tool("slow", json!({ "text": "x" })));
  match tokio::time::timeout(Duration::from_millis(200), slow).await {
    Err(_) => println!("  slow call cancelled after 200 ms"),
    Ok(r) => println!("  slow call finished unexpectedly: {r:?}"),
  }
  Ok(())
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
  let args: Vec<String> = std::env::args().skip(1).collect();
  let flag = |name: &str| {
    args
      .iter()
      .position(|a| a == name)
      .and_then(|i| args.get(i + 1))
      .cloned()
  };
  let outcome = match flag("--url") {
    Some(url) => {
      let mut http = HttpClientConfig::default();
      if url.starts_with("http://127.0.0.1") || url.starts_with("http://localhost") {
        // Explicitly allow the local host (the SSRF policy refuses loopback by default).
        http = http.allow_hosts(["127.0.0.1", "localhost"]);
      }
      let mut client = McpClient::http(url, &http);
      if let Some(token) = flag("--token") {
        client = match client.with_bearer_token(&token) {
          Ok(c) => c,
          Err(e) => {
            eprintln!("mcp_client: {e}");
            return std::process::ExitCode::FAILURE;
          }
        };
      }
      enrich(&client).await
    }
    None => {
      let (client_io, server_io) = tokio::io::duplex(1 << 20);
      let (sr, sw) = tokio::io::split(server_io);
      tokio::spawn(
        TokioRuntime::current()
          .expect("tokio")
          .run(fixture_server().serve_io(sr, sw)),
      );
      let (cr, cw) = tokio::io::split(client_io);
      match run(McpClient::over_io(cr, cw)).await {
        Ok(client) => enrich(&client).await,
        Err(e) => Err(e),
      }
    }
  };
  match outcome {
    Ok(()) => std::process::ExitCode::SUCCESS,
    Err(e) => {
      eprintln!("mcp_client: {e}");
      std::process::ExitCode::FAILURE
    }
  }
}
