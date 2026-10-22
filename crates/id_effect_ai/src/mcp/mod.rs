//! Model Context Protocol (`feature = "mcp"`; Streamable HTTP with `mcp-http`): an effect-native
//! server and client pinned to one specification revision (ADR 0015 §7), serving the same tools
//! the agent loop uses.
//!
//! **Pinned revision: [`PROTOCOL_VERSION`] (`2026-07-28`).** The revision is stateless:
//!
//! - There is no `initialize` handshake and no protocol session. Capability negotiation is
//!   `server/discover` (the server's revisions, capabilities and identity) plus the `_meta` of
//!   every request (the revision, the client's identity and capabilities). A request for another
//!   revision is answered with [`UNSUPPORTED_PROTOCOL_VERSION`] listing the pinned one; one
//!   without the required `_meta` with [`INVALID_PARAMS`]. Clients that only speak 2025-11-25 or
//!   older are not served in 0.13.
//! - **Tools** come from a [`ToolSet`](crate::tools::ToolSet): the same `Tool` values the agent
//!   loop runs, under the same rules (registered names only, arguments decoded through the
//!   tool's `Schema`, output capped at 64 KiB, write tools only under
//!   [`McpServer::dangerous_mcp_write_tools`]). A tool's own failure is a result with `isError`,
//!   which a model can read; an unknown tool is [`INVALID_PARAMS`].
//! - **Resources** (`resources/list`, `resources/read`) and **prompts** (`prompts/list`,
//!   `prompts/get`) are registered with a reader or renderer effect.
//! - **Cancellation** interrupts the request's fiber and runs its finalizers: over stdio
//!   `notifications/cancelled` names the request, over Streamable HTTP the client closes the
//!   response stream. **Progress** ([`report_progress`]) flows to a client that sent a
//!   `progressToken`: as notifications over stdio, as events on the request's own response
//!   stream over HTTP.
//! - Every identifier the server mints and later accepts back (list cursors, tool handles from
//!   [`mint_handle`]) is 128-bit random and bound to the principal; another principal presenting
//!   it is refused exactly like an unknown one.
//! - Not offered in 0.13: `subscriptions/listen`, server-initiated requests (sampling,
//!   elicitation, roots) and the tasks extension; the client advertises none of them.
//!
//! **Transports.** stdio ([`McpServer::serve_stdio`], [`McpServer::serve_io`],
//! [`McpClient::over_io`]): newline-delimited JSON-RPC. Streamable HTTP (`mcp-http`):
//! `McpEndpoint`, an `id_effect_axum` route that needs a guard (ADR 0013 R1), checks `Origin`
//! (R9) and caps messages at 1 MiB (R4); [`McpClient::http`] sends through
//! `id_effect_platform`'s client, so its SSRF policy applies (R6).
//!
//! Every message is decoded by [`decode_message`] (bounded, never panics; the
//! `mcp_jsonrpc_message` fuzz target). `rmcp` is the interop peer in tests, in both directions,
//! and is not a dependency.
//!
//! ```
//! use id_effect::{Effect, Runtime, Schema, succeed};
//! use id_effect_ai::mcp::{McpClient, McpServer};
//! use id_effect_ai::tools::{Tool, ToolSet};
//!
//! #[derive(Clone, Debug, Schema)]
//! struct Add { a: i64, b: i64 }
//! #[derive(Clone, Debug, Schema)]
//! struct Sum { sum: i64 }
//!
//! let tools = ToolSet::new()
//!   .with(Tool::new("add", "Adds two integers", |i: Add| -> Effect<Sum, String> {
//!     succeed(Sum { sum: i.a + i.b })
//!   }))
//!   .unwrap();
//! let server = McpServer::new("calc", "1.0.0").with_tools(tools);
//!
//! // Server and client over an in-memory pipe (a child process's stdio works the same way).
//! let (client_io, server_io) = tokio::io::duplex(64 * 1024);
//! let (server_read, server_write) = tokio::io::split(server_io);
//! let _serving = Runtime::global().fork(server.serve_io(server_read, server_write));
//! let (client_read, client_write) = tokio::io::split(client_io);
//! let sum = McpClient::over_io(client_read, client_write)
//!   .flat_map(|client| client.call_tool("add", serde_json::json!({ "a": 2, "b": 3 })))
//!   .block_on()
//!   .into_result()
//!   .unwrap();
//! assert_eq!(sum.structured_content, Some(serde_json::json!({ "sum": 5 })));
//! ```

mod client;
mod context;
mod error;
mod headers;
#[cfg(feature = "mcp-http")]
mod http;
mod message;
mod server;
mod stdio;
mod types;

pub use client::{DEFAULT_MAX_STREAM_BYTES, MAX_LIST_PAGES, McpClient, ProgressFn};
pub use context::{HandleError, current_principal, mint_handle, report_progress, resolve_handle};
pub use error::McpError;
pub use headers::{HEADER_METHOD, HEADER_NAME, HEADER_PARAM_PREFIX, HEADER_PROTOCOL_VERSION};
#[cfg(feature = "mcp-http")]
pub use http::{DEFAULT_PATH, EndpointAccess, Guarded, McpEndpoint, Unguarded};
pub use message::{
  DEFAULT_MAX_MESSAGE_BYTES, DecodeError, ErrorResponse, HEADER_MISMATCH, INTERNAL_ERROR,
  INVALID_PARAMS, INVALID_REQUEST, JSONRPC_VERSION, MAX_NAME_BYTES, METHOD_NOT_FOUND,
  MISSING_REQUIRED_CLIENT_CAPABILITY, Message, Notification, PARSE_ERROR, Request, RequestId,
  Response, RpcError, SERVER_BUSY, UNSUPPORTED_PROTOCOL_VERSION, decode_message,
  decode_message_with_limit,
};
pub use server::{DANGEROUS_MCP_WRITE_TOOLS, McpLimits, McpServer};
pub use types::{
  Content, Implementation, McpTool, Progress, Prompt, PromptArgument, PromptMessage, PromptReply,
  PromptRole, Resource, ResourceBody, ResourceContents, ServerDescription, ToolResult,
};

/// The MCP specification revision this implementation speaks (ADR 0015 §7).
pub const PROTOCOL_VERSION: &str = "2026-07-28";

/// The principal every stdio request acts for: whoever started the process.
pub const STDIO_PRINCIPAL: &str = "stdio";

/// Request `_meta` key: the protocol revision.
pub const META_PROTOCOL_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
/// Request `_meta` key: the client's identity.
pub const META_CLIENT_INFO: &str = "io.modelcontextprotocol/clientInfo";
/// Request `_meta` key: the client's capabilities.
pub const META_CLIENT_CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";
/// Result `_meta` key: the server's identity (on `server/discover`).
pub const SERVER_INFO_META_KEY: &str = "io.modelcontextprotocol/serverInfo";

/// The progress notification method.
pub const NOTIFICATION_PROGRESS: &str = "notifications/progress";
/// The cancellation notification method.
pub const NOTIFICATION_CANCELLED: &str = "notifications/cancelled";
