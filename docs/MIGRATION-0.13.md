# Migrating to id_effect 0.13

This guide is the "if you used X, now do Y" companion to the `0.13.0` section of
[CHANGELOG.md](../CHANGELOG.md). It assumes you are on 0.12. If you are on 0.11, apply
[MIGRATION-0.12.md](MIGRATION-0.12.md) first.

Every intentional public-API break in 0.13 is listed here, each with its shim (rule L9) or the
reason it has none. A break reported by `cargo semver-checks` (the `semver` CI job, baseline the
last 0.12.x release) that is not listed below is a bug. The deprecation rule and semver policy are
in [STABILITY.md](STABILITY.md).

0.13 is the "Breadth" release: AI (`id_effect_ai`, promoted from labs), an admin back office
(`id_effect_admin`), server-rendered UI (`id_effect_axum` feature `ui`), i18n (`id_effect_i18n`),
ORM relations, keyset pagination and lifecycle hooks, and tenancy helpers.

Each section below names the leaf that owns it; the leaf writes its body when it lands, and
`r13-t0-migration-guide` checks the list against `cargo semver-checks` at the release exit.

## SQL: `id_effect_sql::repository` split into a module tree

Owner: r13-e3-3-relations. The paths of 0.12 stay importable through `pub use`.

## SQL: `Entity` gains associated items

Owners: r13-e3-3-relations, r13-e3-3-hooks. Every new item has a default, so manual `Entity`
implementations keep compiling.

## Events: `EventLog::head`

Owner: r13-f-eventlog-head. A provided method; implementors outside the workspace need no change.

## Axum: `WsConfig` control-frame cap

Owner: r13-f-ws-credit.

## Axum / platform: `Http3Options` early-data and congestion settings

Owner: r13-f-h3-settings. No signature breaks (new items only; `Refusal` and `EndpointStats`
are `#[non_exhaustive]`); two behaviour changes:

- `Http3Options::dangerous_allow_zero_rtt_for_queries` is no longer a recorded no-op: the `h3`
  route now takes 0-RTT early data (`Route::dangerous_allow_early_data`, also recorded) and
  serves early `GET` / `HEAD` requests before the handshake completes; every other early
  request is answered `425 Too Early`. `Http3Options::admits_early` no longer admits `OPTIONS`.
  If you set the opt-out "for later", remove it unless your safe-method handlers are idempotent
  and reveal nothing a replay could exploit.
- `AlpnRoute::accept` on a route with early data waits for each connection's handshake; use
  `AlpnRoute::accept_early` to receive connections at once with their `Handshake`. Routes
  without early data (the default, and always the mesh) are unchanged.

New: `Http3Options::congestion(CongestionControl::Cubic)` (default stays BBR) and
`Http3Options::config()` for the config key `server.http3.congestion`; `CongestionControl`
implements `FromStr` (`bbr`, `cubic`).

## RPC: `HttpTransport::with_header` / `with_bearer`

Owner: r13-f-client-batching. Additive: `HttpTransport::with_header`, `with_bearer`,
`service::HttpHeaderError` and the provided method `RpcTransport::unary_effect` are new; code that
implements `RpcTransport` or builds an `HttpTransport` compiles unchanged. A wrapper
`HttpClient` that only added an `authorization` header can go:

```rust,ignore
// before (0.12): a custom HttpClient that pushed the header onto every HttpRequest
// after (0.13)
let transport = HttpTransport::new(RpcClient::new(url), http)
  .with_bearer(id_effect_config::Secret::new(token))?;
```

Behavior: a generated client over `HttpTransport` now sends the calls it makes in the same tick
(the children of one `all`, for example) as one batch request, like `RpcClient::call`. A server
whose `BatchConfig` caps a batch below 50 calls or 1 MiB refuses a larger window with its
Problem, given to each call of it; raise the server caps or call through
`RpcTransport::unary`, which still sends one call per request.

## RPC / node: session pump, `MeshRpcError::PeerSilent`

Owner: r13-f-mesh-session. One break, two behaviour changes; everything else is additive
(`id_effect_node::session_pump`, `MeshLiveness`, `MeshRpc::{with_liveness, liveness, heartbeat,
of}`, `MeshTransport::{in_current_span, with_trace_context, trace_context}`, `SessionLink::stats`).

- `id_effect_rpc::client::mesh::MeshRpcError` gains the variant `PeerSilent { node, silent_for }`
  (the enum is not `#[non_exhaustive]`), so an exhaustive `match` needs one more arm. No shim:
  the error is new behaviour. Treat it like `NodeDown` (`MeshRpcError::is_endpoint_gone` is true
  for it):

  ```rust,ignore
  match error {
    MeshRpcError::NodeDown | MeshRpcError::PeerSilent { .. } => reconnect_later(),
    other => fail(other),
  }
  ```

- Mesh streams (server streams, subscriptions, two-way streams) now heartbeat. A stream whose
  endpoint's node answers nothing for 3 × 5 s fails with `PeerSilent` even when no `peer_down` was
  reported. Tune it with `MeshRpc::with_liveness(MeshLiveness::new().every(..).silent_after(..))`;
  the interval and the deadline are clamped finite. An endpoint on 0.12 does not answer the
  heartbeats: upgrade the serving nodes before (or with) the calling ones.
- `channels::attach_session` on a bare mesh now delivers through the node's `MeshRpc` when one is
  built over that mesh, so mesh RPC calls over the link run instead of being dropped as
  unverified; and every envelope a session carries is checked against the session's peer (an
  envelope naming a third node's process, or claiming a third node's name, is dropped and counted
  in `SessionLink::stats`). Code that relied on `attach_session` dropping RPC calls should not
  build a `MeshRpc` over that mesh.

## Tool: `rpc::check::CheckArgs::allow_retired`

Owner: r13-f-rpc-retire. `id_effect_tool::rpc::check::CheckArgs` (the clap arguments of
`id-effect rpc check`) gains the public field `allow_retired: Vec<RetiredVersion>` for the new
`--allow-retired Service:N` flag. The command line is unchanged for existing invocations. Code
that builds `CheckArgs` with a struct literal adds the field:

```rust,ignore
// before (0.12)
let args = CheckArgs { contract, against: Some(spec), generated: None };
// after (0.13)
let args = CheckArgs { contract, against: Some(spec), generated: None, allow_retired: Vec::new() };
```

No shim: a struct literal cannot default a field. Parsing arguments with clap (`CheckArgs` derives
`Args`) needs no change.

## TLS: rcgen 0.14

Owner: r13-f-rcgen. Only a break if an rcgen type appears in a public signature.

## CLI: `crates/id_effect_cli/templates/admin-stub` removed

Owner: r13-t0-skeletons. The `admin-stub` directory shipped as unused files in the `id_effect_cli`
package; no API, command or template ever read it (`id-effect new` uses its own templates in
`crates/id_effect_tool/templates/`). Nothing to change in code. For an admin, use the new
`id_effect_admin` crate (ADR 0010 Amendment 9) and `id-effect generate admin <Entity>`.

## AI: `id_effect_ai` published from `crates/` (was `labs/`)

Owner: r13-e9-1-promote. The crate moved from `labs/id_effect_ai` to `crates/id_effect_ai` (ADR 0010
Amendment 9) and is a root-workspace member. A path dependency on the labs crate changes its path:

```toml
# before (0.12, labs)
id_effect_ai = { path = "../id_effect/labs/id_effect_ai" }
# after (0.13)
id_effect_ai = { path = "../id_effect/crates/id_effect_ai" }   # or: id_effect_ai = "0.13"
```

The API changes of the promotion (the labs crate had no semver promise; each has its
replacement):

| 0.12 (labs) | 0.13 |
|---|---|
| `ChatRequest { model, messages }` literal | `ChatRequest::new(model, messages)` or `ChatRequest::user(model, text)`; the struct is `#[non_exhaustive]` and gains `max_tokens: Option<u32>` (`with_max_tokens`) |
| `ChatMessage { role, content }` | still valid; also `ChatMessage::system` / `user` / `assistant` |
| `ChatResponse { content, tokens_used }`, `resp.tokens_used` | `ChatResponse::text(..)` (`#[non_exhaustive]`); `resp.usage.total()` (`Usage { input_tokens, output_tokens }`), plus `stop_reason` and `tool_calls` |
| `AiConfig { openai_api_key: Some(..), .. }` literal | `AiConfig::default().with_openai_api_key(..).with_openai_base_url(..)` (and the Anthropic, Ollama and Cursor twins); fields stay public for reading and assignment |
| `AiConfig::anthropic_max_tokens`, `ANTHROPIC_MAX_TOKENS` | `AiConfig::limits.max_tokens` (`AiLimits`), now sent by both vendors; `AI_MAX_TOKENS` (the old variable is still read) |
| `provide_*_language_model(client, config)` with any platform `HttpClient` | unchanged; `config.http_client()` builds the guarded client from `AiConfig::http` (keep a local mock or Ollama reachable with `HttpClientConfig::allow_hosts`, never by turning the SSRF guard off). Each request now carries `AiLimits::request_timeout` and a body cap |
| `id_effect_ai::model::*`, `id_effect_ai::streaming::*`, `id_effect_ai::sse::*` (private modules, root re-exports) | `id_effect_ai::chat::*` and `id_effect_ai::stream::*`; the root re-exports are unchanged |
| `id_effect_ai::vendors::openai::*`, `vendors::anthropic::*` | `vendors::openai::chat::*`, `vendors::anthropic::chat::*` (re-exported from `vendors::openai` / `vendors::anthropic` and the crate root as before) |
| `AiError` (exhaustive) | `#[non_exhaustive]`, with `InvalidRequest`, `Blocked`, `LimitExceeded` and `Timeout`; HTTP 529 is `Transient` |
| `SseParser::feed` trimmed every leading space of a value | one leading space is removed (the SSE rule); `feed_bytes` takes raw bytes |
| streamed chunks parsed after the whole body was read | decoded incrementally, under `AiLimits::max_stream_bytes` / `max_stream_events` |

## AI: tool calling in `ChatMessage`, `ChatRole` and `ChatRequest`

Owner: r13-e9-1-tools. Tool calling (feature `tools`) needs the conversation to carry tool calls
and their results, so the chat types grow (the labs crate had no semver promise; this supersedes
the "`ChatMessage { role, content }` still valid" row above):

| 0.12 (labs) | 0.13 |
|---|---|
| `ChatMessage { role, content }` literal | `ChatMessage::new(role, content)` or `ChatMessage::system` / `user` / `assistant`; the struct is `#[non_exhaustive]` and gains `tool_calls: Vec<ToolCall>` (`assistant_tool_calls`) and `tool_call_id: Option<String>` (`tool_result`); both are skipped in serde when empty, so stored JSON of plain messages is unchanged |
| `match role { System, User, Assistant }` | add an arm for `ChatRole::Tool` (a tool result fed back to the model) |
| `ChatRequest` without tools | gains `tools: Vec<ToolDefinition>` (`with_tools`; empty by default, so no request changes); `validate` refuses a `ChatRole::Tool` message without `tool_call_id` |
| `ChatResponse` text only | a reply with tool calls and no text is no longer `AiError::EmptyResponse`: `content` is empty and `tool_calls` is set (both vendors) |
