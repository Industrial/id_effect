# ADR: `id_effect_ai` vendor layout

**Status:** Accepted  
**Date:** 2026-06-19  
**Context:** Phase H (`iep-h-010`) — multi-vendor LLM clients for `id_effect`.

## Decision

1. **Single crate** `id_effect_ai` with Cargo **features**: `openai`, `anthropic`, `cursor`, `full`.
2. **Transport:** all vendors use `id_effect_platform::HttpClient` (no direct `reqwest` in vendor code).
3. **OpenAI + ChatGPT:** one `openai` feature targeting OpenAI **Chat Completions** (`POST /v1/chat/completions`). ChatGPT models are model ID strings (`gpt-4o`, `gpt-4.1`, etc.).
4. **Anthropic Claude:** `anthropic` feature targeting **Messages API** (`POST /v1/messages`) with SSE streaming.
5. **Cursor:** `cursor` feature exposes **`CursorAgentsClient`** (Cloud Agents API v1) — **not** `LanguageModel`.
6. **Secrets:** API keys as `id_effect_config::Secret<String>`; load via `AiConfig::from_env()`.
7. **Retries:** `Schedule`-based wrapper retries 429/502/503/504 only.
8. **MSRV:** matches workspace (edition 2024).

## Module tree

```
crates/id_effect_ai/src/
  lib.rs
  error.rs
  model.rs
  streaming.rs
  config.rs
  retry.rs
  http_util.rs
  sse.rs
  tracing_util.rs
  vendors/
    mod.rs
    openai.rs
    anthropic.rs
  cursor/
    mod.rs
    types.rs
    agents.rs
    models.rs
```

## Non-goals (v1)

- OpenAI Responses API (`/v1/responses`)
- Tool-calling framework
- Vendor SDK dependencies

## References

- [phase-h-ai.md](../../effect-ts-parity/phases/phase-h-ai.md)
- [adr-ai-threat-model.md](./adr-ai-threat-model.md)
