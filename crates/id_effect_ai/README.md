# id_effect_ai

Provider-neutral LLM chat, streaming and deterministic test doubles for `id_effect`, secure by
default. Tier `integration` ([ADR 0010](../../docs/adrs/0010-crate-tiers.md) Amendment 9).

## Features

| Cargo feature | What | API |
|---------------|------|-----|
| `openai` | OpenAI / ChatGPT models, OpenAI-compatible servers | `OpenAiLanguageModel` (`/v1/chat/completions`) |
| `anthropic` | Claude | `AnthropicLanguageModel` (`/v1/messages`) |
| `ollama` | Ollama (embeddings) | `vendors::ollama` |
| `cursor` | Cursor Cloud Agents (non-default) | `CursorAgentsClient` |
| `test-util` | Deterministic doubles | `testing::ScriptedModel` |
| `tools`, `embeddings`, `mcp`, `mcp-http` | Tool calling, embeddings, MCP | `tools`, `embeddings`, `mcp` |
| `full` | All of the above but `test-util` | — |

## Quick start

```rust
use id_effect::{HCons, HNil};
use id_effect_ai::{
  AiConfig, AiError, ChatRequest, LanguageModelService, complete, provide_openai_language_model,
};

let cfg = AiConfig::from_env();
let layer = provide_openai_language_model::<AiError>(cfg.http_client(), cfg)?;
// `id_effect` effect; `block_on` at the program edge (or `TokioRuntime::run` in async code).
let resp = complete::<HCons<LanguageModelService, HNil>, _>(ChatRequest::user("gpt-4o-mini", "Hello"))
  .provide_layer(layer)
  .block_on()
  .into_result();
```

Streaming: `complete_stream` returns an `id_effect::Stream<CompletionChunk, AiError>`, decoded
incrementally from the vendor's server-sent events.

## Secure by default (ADR 0013)

- Every HTTP call goes through `id_effect_platform`'s client: the SSRF policy refuses loopback,
  private, link-local and metadata addresses (a self-hosted endpoint is allowed explicitly with
  `HttpClientConfig::allow_hosts` in `AiConfig::http`), redirects are not followed.
- API keys are `id_effect_config::Secret`: `Debug` prints `<redacted>`.
- Prompts and completions are never logged or put on spans; `AiConfig::dangerous_log_prompts()`
  turns that on and records itself in the posture registry.
- `AiLimits`: `max_tokens` 1024 by default on every request, a 60 s request timeout, a 4 MiB
  response cap, and 16 MiB / 65 536 events per stream; a stream over a cap fails with
  `AiError::LimitExceeded` and is closed.

## Testing without a vendor

`testing::ScriptedModel` (feature `test-util`) answers from a script: replies, tool calls, streams
and failures, in order, and records every request. CI never calls a vendor (ADR 0015 D8); the
vendor adapters are tested against wiremock fixtures.

## Environment variables

| Variable | Purpose |
|----------|---------|
| `OPENAI_API_KEY` | OpenAI / ChatGPT |
| `ANTHROPIC_API_KEY` | Anthropic Claude |
| `CURSOR_API_KEY` | Cursor Cloud Agents |
| `OPENAI_BASE_URL` | Optional OpenAI-compatible base URL |
| `ANTHROPIC_BASE_URL` | Optional Anthropic base URL |
| `OLLAMA_BASE_URL` | Ollama base URL (default `http://localhost:11434`) |
| `CURSOR_BASE_URL` | Optional Cursor API base (default `https://api.cursor.com`) |
| `AI_MAX_TOKENS` | Default `max_tokens` (`ANTHROPIC_MAX_TOKENS` is still read) |

## Examples

```bash
cargo run -p id_effect_ai --features test-util --example 005_scripted_model
cargo run -p id_effect_ai --features openai --example 010_openai_once -- "Hello"
cargo run -p id_effect_ai --features anthropic --example 011_anthropic_once -- "Hello"
cargo run -p id_effect_ai --features openai,anthropic --example ask_once -- --vendor openai --prompt "Hi"
cargo run -p id_effect_ai --features cursor --example 020_cursor_agent_once -- "Summarize README"
cargo run -p id_effect_ai --features mcp,mcp-http --example mcp_server -- --self-test
cargo run -p id_effect_ai --features mcp,mcp-http --example mcp_client
```

## Testing

```bash
cargo nextest run -p id_effect_ai --all-features
```

The book chapter is "AI and MCP" (part 6, chapter 35).
