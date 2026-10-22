# MCP: Serving and Calling Tools

The Model Context Protocol (MCP) lets an AI host (a desktop assistant, an IDE, another agent)
discover and call tools, read resources and render prompts on a server it did not compile in.
`id_effect_ai` implements both sides natively: features `mcp` (server, client, stdio) and
`mcp-http` (the Streamable HTTP endpoint, an `id_effect_axum` route).

It is pinned to one specification revision, **2026-07-28** (`mcp::PROTOCOL_VERSION`), recorded
in ADR 0015 §7. That revision is stateless, which shapes everything below: there is no
`initialize` handshake and no protocol session. A client learns what a server offers with
`server/discover`, and every request carries the revision, the client's identity and its
capabilities in `_meta`. A request for another revision is answered with
`UnsupportedProtocolVersion` (`-32022`), listing the one the server speaks.

| Item | What it is |
|------|------------|
| `mcp::McpServer` | Tools (a `ToolSet`), resources and prompts; `serve_stdio`, `serve_io` |
| `mcp::McpEndpoint` | The server as a guarded Streamable HTTP route (`mcp-http`) |
| `mcp::McpClient` | Calls a server over stdio (`over_io`) or HTTP (`http`) |
| `mcp::report_progress`, `mint_handle`, `resolve_handle` | What a tool can do inside an MCP request |
| `mcp::decode_message` | The bounded JSON-RPC decoder every transport uses (a fuzz target) |

## Tools are the agent loop's tools

A server serves a `ToolSet`, the same value the agent loop of the previous chapter uses. Every
call goes through `ToolSet::invoke`, so the rules do not change with the transport: only
registered tools run (an unknown name is `InvalidParams`), arguments are decoded through the
tool's `Schema` before the handler runs, output is capped at 64 KiB, and a tool that writes is
neither listed nor run unless the server takes `dangerous_mcp_write_tools()` (recorded in the
posture registry, so the prod posture check sees it). A handler's own failure becomes a result
with `isError: true`, which the calling model can read and react to.

```rust
use std::collections::BTreeMap;

use id_effect::{Effect, Runtime, Schema, succeed};
use id_effect_ai::mcp::{
  McpClient, McpServer, Prompt, PromptArgument, PromptMessage, Resource, ResourceBody,
};
use id_effect_ai::tools::{Tool, ToolSet};

#[derive(Clone, Debug, Schema)]
struct Query { query: String }
#[derive(Clone, Debug, Schema)]
struct Hits { titles: Vec<String> }

let search = Tool::new("search_documents", "Finds documents", |q: Query| -> Effect<Hits, String> {
  let titles = ["Invoice 001", "Contract"]
    .iter()
    .filter(|t| t.to_lowercase().contains(&q.query.to_lowercase()))
    .map(|t| t.to_string())
    .collect();
  succeed(Hits { titles })
});
let server = McpServer::new("documents", "1.0.0")
  .with_tools(ToolSet::new().with(search).unwrap())
  .with_resource(Resource::new("docs://index", "index"), || {
    succeed::<_, String, _>(ResourceBody::Text("Invoice 001\nContract".into()))
  })
  .with_prompt(
    Prompt::new("summarize").with_argument(PromptArgument::required("title", "the document")),
    |args: BTreeMap<String, String>| {
      succeed::<_, String, _>(vec![PromptMessage::user(format!("Summarize {}", args["title"]))])
    },
  );

// A client over an in-memory pipe; a child process's stdin/stdout works the same way.
let (client_io, server_io) = tokio::io::duplex(1 << 16);
let (server_read, server_write) = tokio::io::split(server_io);
let _serving = Runtime::global().fork(server.serve_io(server_read, server_write));
let (client_read, client_write) = tokio::io::split(client_io);
let client = McpClient::over_io(client_read, client_write).block_on().into_result().unwrap();

let found = client
  .call_tool("search_documents", serde_json::json!({ "query": "invoice" }))
  .block_on()
  .into_result()
  .unwrap();
assert_eq!(found.structured_content, Some(serde_json::json!({ "titles": ["Invoice 001"] })));
let index = client.read_resource("docs://index").block_on().into_result().unwrap();
assert_eq!(index[0].text(), Some("Invoice 001\nContract"));
```

`serve_stdio()` is the same server on the process's own stdin and stdout, which is how desktop
hosts launch MCP servers. Keep logs on stderr: stdout carries the protocol.

## Inside a request: progress, handles, cancellation

A tool is an ordinary effect, but while it runs inside an MCP request it can reach the request
through three functions:

- `report_progress(progress, total, message)` sends a `notifications/progress` when the client
  asked for progress (a `progressToken`); otherwise it does nothing. `McpClient` receives them
  with `call_tool_with_progress`.
- `mint_handle(value)` stores a value and returns a 128-bit random id **bound to the
  principal** of the request; `resolve_handle(id)` returns it to the same principal only. This
  is how a tool keeps state across calls (an open cart, a running import) without a protocol
  session. Another principal presenting the id gets `unknown handle`, exactly like a made-up
  id, so a handle cannot be stolen or probed. List cursors are minted the same way.
- Cancellation interrupts the request's fiber, so the tool's finalizers (`ensuring`,
  `acquire_release`) run. Over stdio the client sends `notifications/cancelled`; over HTTP it
  closes the response stream. `McpClient` does either when the call's effect is interrupted or
  dropped, so a timeout around a call cancels it on the server.

```rust
use id_effect::{Effect, Schema};
use id_effect_ai::mcp::{mint_handle, report_progress, resolve_handle};
use id_effect_ai::tools::Tool;

#[derive(Clone, Debug, Schema)]
struct Item { name: String }
#[derive(Clone, Debug, Schema)]
struct Handle { handle: String }

let open = Tool::new("open_cart", "Opens a cart", |i: Item| -> Effect<Handle, String> {
  report_progress(0.5, Some(1.0), Some("reserving".into()))
    .flat_map(move |()| mint_handle(serde_json::json!({ "item": i.name })).map_err(|e| e.to_string()))
    .map(|handle| Handle { handle })
});
let read = Tool::new("read_cart", "Reads a cart", |h: Handle| -> Effect<Item, String> {
  resolve_handle(h.handle)
    .map(|v| Item { name: v["item"].as_str().unwrap_or_default().to_owned() })
    .map_err(|e| e.to_string())
});
# let _ = (open, read);
```

## Streamable HTTP: a guarded route

`McpEndpoint` mounts the server as one `POST` route of an `id_effect_axum` typed router. It
follows ADR 0013 the way every other surface does:

- **No endpoint without an access decision.** An endpoint has no `router()` until `.guard(g)`
  attached a route guard (an `id_effect_authz::Guard` or any `RouteGuard`); forgetting it does
  not compile. The principal the guard admitted is the one handles are bound to, and an
  anonymous principal is refused with `401` even behind a permissive guard.
- **Origin allowlist.** A request carrying `Origin` must match the endpoint's `OriginPolicy`,
  empty by default, so a web page cannot drive a local MCP server through the user's browser
  (DNS rebinding). Clients that are not browsers send no `Origin` and pass.
- **Bounded.** A body over 1 MiB is refused with `413` before it is parsed, and each principal
  has at most 16 requests running (`429` beyond); both are `McpLimits`.
- **Headers agree with the body.** `MCP-Protocol-Version` must name the pinned revision, and the
  routing headers `Mcp-Method` and `Mcp-Name` (and `Mcp-Param-*` for tool arguments a schema
  marks with `x-mcp-header`) must match the JSON-RPC body, or the request is refused with
  `HeaderMismatch` (`-32020`). Intermediaries that route on headers cannot be steered by a
  different body.

```rust
use id_effect_ai::mcp::{McpEndpoint, McpServer};
use id_effect_authz::{Guard, require_role};
use id_effect_axum::realtime::origin::OriginPolicy;

let router = McpEndpoint::new(McpServer::new("documents", "1.0.0"))
  .allow_origins(OriginPolicy::allow(["https://app.example.com"]).unwrap())
  .guard(Guard::new("mcp.agent", require_role("agent")))
  .router::<()>();
assert!(router.access_report().public.is_empty());
```

A request whose `_meta` carries a `progressToken` is answered as server-sent events (progress
notifications, then the response); any other as one JSON body.

## The client

`McpClient::http(url, &HttpClientConfig)` sends every request through `id_effect_platform`'s
client, so the SSRF policy applies: private, loopback and link-local addresses are refused
(`McpError::Blocked`) unless the configuration allows the host explicitly. Credentials go in
with `with_bearer_token` or `with_header`; the headers the protocol owns (`content-type`,
`accept`, `mcp-*`) cannot be set or overridden.

```rust
use id_effect_ai::mcp::{McpClient, McpError};
use id_effect_platform::http::HttpClientConfig;

// The default policy refuses loopback: nothing is sent.
let local = McpClient::http("http://127.0.0.1:9/mcp", &HttpClientConfig::default());
let refused = local.discover().block_on().into_result().unwrap_err();
assert!(matches!(refused.fail_option(), Some(McpError::Blocked(_))));
assert!(local.clone().with_header("mcp-method", "tools/list").is_err());
```

`list_tools`, `list_resources` and `list_prompts` follow cursors (at most 64 pages);
`call_tool` returns a `ToolResult` whose `is_error` tells a tool failure from a protocol error
(`McpError::Rpc`).

## What is not in 0.13

`subscriptions/listen` (change notifications), server-initiated requests (sampling,
elicitation, roots) and the tasks extension are not offered, and the client advertises none of
them. Peers that only speak 2025-11-25 or older are refused with the version error; a
compatibility path would be an additive change. Interop is tested in both directions against
`rmcp` (a dev-dependency only), over stdio and Streamable HTTP.

The examples `mcp_server` (stdio, `--http`, `--self-test`) and `mcp_client` in
`crates/id_effect_ai/examples` put the pieces together.
