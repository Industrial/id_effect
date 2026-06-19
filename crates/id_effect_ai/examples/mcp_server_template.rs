//! MCP server template — JSON-RPC line protocol over stdio (no network).
//!
//! Run: `cargo run -p id_effect_ai --example mcp_server_template`
//!
//! Send one line: `{"jsonrpc":"2.0","id":1,"method":"tools/list"}`

use id_effect::{Effect, succeed};
use id_effect_ai::{ChatMessage, ChatRequest, ChatRole, LanguageModel, MockLanguageModel};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{self, BufRead, Write};

#[derive(Debug, Deserialize)]
struct RpcRequest {
  jsonrpc: String,
  id: Value,
  method: String,
}

#[derive(Debug, Serialize)]
struct RpcResponse<'a> {
  jsonrpc: &'a str,
  id: &'a Value,
  result: Value,
}

fn handle_line(line: &str) -> Result<String, String> {
  let req: RpcRequest = serde_json::from_str(line).map_err(|e| e.to_string())?;
  if req.jsonrpc != "2.0" {
    return Err("expected jsonrpc 2.0".into());
  }
  let result = match req.method.as_str() {
    "tools/list" => {
      serde_json::json!({"tools": [{"name": "echo", "description": "Echo user text via MockLanguageModel"}]})
    }
    "tools/call" => {
      let model = MockLanguageModel::echo();
      let chat = ChatRequest {
        model: "mock".into(),
        messages: vec![ChatMessage {
          role: ChatRole::User,
          content: "mcp".into(),
        }],
      };
      let resp = id_effect::run_blocking(model.complete(chat), ()).map_err(|e| e.to_string())?;
      serde_json::json!({"content": [{"type": "text", "text": resp.content}]})
    }
    other => return Err(format!("unknown method: {other}")),
  };
  let out = RpcResponse {
    jsonrpc: "2.0",
    id: &req.id,
    result,
  };
  serde_json::to_string(&out).map_err(|e| e.to_string())
}

fn main() -> io::Result<()> {
  let stdin = io::stdin();
  let mut stdout = io::stdout();
  for line in stdin.lock().lines() {
    let line = line?;
    if line.trim().is_empty() {
      continue;
    }
    match handle_line(&line) {
      Ok(resp) => {
        writeln!(stdout, "{resp}")?;
        stdout.flush()?;
      }
      Err(e) => eprintln!("error: {e}"),
    }
  }
  let _: Effect<(), (), ()> = succeed(());
  println!("mcp_server_template ready (stdio JSON-RPC)");
  Ok(())
}
