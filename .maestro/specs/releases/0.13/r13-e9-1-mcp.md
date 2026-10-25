---
title: 'E9.1 — MCP server and client (tools, resources, prompts; stdio and Streamable HTTP): r13-e9-1-mcp'
slug: r13-e9-1-mcp
mode: heavy
work_type: spec-slice
risk_class: high
version: 1
acceptance_criteria:
  - 'ai.llm #6 (D7): an effect-native MCP server and client pinned to one specification revision (the newest stable at leaf start, recorded in ADR 0015 and the module docs; 2025-06-18 is the floor): initialize and capability negotiation, tools (served from the same `ToolSet` the agent loop uses), resources, prompts, cancellation and progress; transports stdio and Streamable HTTP (an axum route through id_effect_axum, mounted with `.guard(g)`).'
  - 'Interop: `rmcp` is a dev-dependency only (exact pin); tests run the rmcp client against our server and our client against an rmcp server.'
  - 'Secure by default (ADR 0013): the HTTP endpoint needs an access decision (R1; compile-fail case added by r13-e-fuzz-security), checks `Origin` against an allowlist (empty refuses browsers; DNS-rebinding defence, R9), binds 128-bit random session ids to the principal (R1), caps a message at 1 MiB and in-flight requests per session (R4), and is closed in prod unless guarded (R7, posture probe in r13-w5-kit-template); the client uses the platform client, so the SSRF policy applies (R6); tool write policy from r13-e9-1-tools applies to MCP-exposed tools, and granting writes to an MCP-exposed set records `dangerous_mcp_write_tools` (R10).'
  - 'Tests `mcp_tools_round_trip_stdio`, `mcp_streamable_http_round_trip`, `mcp_resources_and_prompts`, `mcp_cancellation_interrupts_tool_fiber`, `mcp_interop_rmcp_client_against_our_server`, `mcp_interop_our_client_against_rmcp_server`, `r9_mcp_foreign_origin_refused`, `mcp_session_bound_to_principal`, `r4_mcp_message_over_cap_rejected`, `mcp_client_obeys_ssrf_policy`, `dangerous_mcp_write_tools_is_recorded`; `mcp::decode_message` public for the fuzz target.'
  - 'Examples `mcp_server.rs` and `mcp_client.rs` replace the toy `examples/mcp_server_template.rs`; book `part6/ch35-03-mcp.md`.'
  - 'Tier: integration. Coverage: `cargo llvm-cov nextest -p id_effect_ai --all-features --summary-only --fail-under-regions 96`.'
  - 'Gate: `cargo nextest run -p id_effect_ai --features mcp,mcp-http,tools,test-util`; `cargo run -p id_effect_ai --features mcp,mcp-http --example mcp_server -- --self-test`; `bash scripts/check-dangerous.sh`'
  - 'Owns: `crates/id_effect_ai/src/mcp/**`, `crates/id_effect_ai/examples/{mcp_server.rs,mcp_client.rs}` (and deletion of `examples/mcp_server_template.rs`), `crates/id_effect_ai/tests/{mcp_stdio.rs,mcp_http.rs,mcp_interop.rs}`, `crates/id_effect/book/src/part6/ch35-03-mcp.md`'
---

# r13-e9-1-mcp

Leaf of `.maestro/specs/releases/0.13.0.md`, section "E9.1 — AI". The release spec's row is binding;
this file exists so Maestro can track the leaf as its own task.

Wave 3 (AI branch, one wave of slack). Blocked by: r13-e9-1-tools. Blocks r13-e-fuzz-security and
r13-w5-kit-template. If the owner answers Open question 4 with "wrap rmcp", this leaf keeps its tests
and security rows and swaps the protocol core for an rmcp adapter.
