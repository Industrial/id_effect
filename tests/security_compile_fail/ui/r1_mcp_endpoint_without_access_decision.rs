// ADR 0013 R1, R7 (0.13 breadth, MCP): the Streamable HTTP endpoint needs an access decision.
// An `McpEndpoint` has a `router` only after `.guard(g)`, and the route-level `Public` marker is
// not a guard: an MCP endpoint cannot be mounted without one.
use id_effect_ai::mcp::{McpEndpoint, McpServer};
use id_effect_kit::axum::route::Public;

fn main() {
  let _ = McpEndpoint::new(McpServer::new("docs", "1.0.0")).router::<()>();
  let _ = McpEndpoint::new(McpServer::new("docs", "1.0.0"))
    .guard(Public)
    .router::<()>();
}
