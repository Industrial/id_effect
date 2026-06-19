# id_effect_ai

Vendor-neutral LLM client traits and multi-vendor HTTP adapters for `id_effect`.

## Features

| Cargo feature | Vendor | API |
|---------------|--------|-----|
| `openai` | OpenAI / ChatGPT models | `LanguageModel` via `/v1/chat/completions` |
| `anthropic` | Claude | `LanguageModel` via `/v1/messages` |
| `cursor` | Cursor Cloud Agents | `CursorAgentsClient` (native agents API) |
| `full` | All of the above | — |

## Quick start

```rust
use id_effect::{build_env, run_async};
use id_effect_ai::{AiConfig, ChatRequest, ChatRole, ChatMessage, complete, provide_openai_language_model};
use id_effect_platform::http::{ReqwestHttpClient, provide_reqwest_http_client};
use std::sync::Arc;

let cfg = AiConfig::from_env();
let client = Arc::new(ReqwestHttpClient::default_client());
let env = build_env([
  provide_reqwest_http_client(),
  provide_openai_language_model(client, cfg)?,
])?;
```

## Environment variables

| Variable | Purpose |
|----------|---------|
| `OPENAI_API_KEY` | OpenAI / ChatGPT |
| `ANTHROPIC_API_KEY` | Anthropic Claude |
| `CURSOR_API_KEY` | Cursor Cloud Agents |
| `OPENAI_BASE_URL` | Optional OpenAI-compatible base URL |
| `ANTHROPIC_BASE_URL` | Optional Anthropic base URL |
| `CURSOR_BASE_URL` | Optional Cursor API base (default `https://api.cursor.com`) |

Secrets use `id_effect_config::Secret` — never logged.

## Examples

```bash
cargo run -p id_effect_ai --features openai --example 010_openai_once -- "Hello"
cargo run -p id_effect_ai --features anthropic --example 011_anthropic_once -- "Hello"
cargo run -p id_effect_ai --features full --example ask_once -- --vendor openai --prompt "Hi"
cargo run -p id_effect_ai --features cursor --example 020_cursor_agent_once -- "Summarize README"
cargo run -p id_effect_ai --example mcp_server_template
```

## Testing

```bash
cargo nextest run -p id_effect_ai --all-features
```

Wiremock integration tests run without live API keys. Optional live tests require env vars.

## Design

See [adr-ai-vendor-layout.md](../../docs/platform/adrs/adr-ai-vendor-layout.md) and [phase-h-ai.md](../../docs/effect-ts-parity/phases/phase-h-ai.md).
