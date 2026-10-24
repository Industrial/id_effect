---
title: 'E9.1 — Promote id_effect_ai to integration: provider-neutral chat, streaming, safe defaults: r13-e9-1-promote'
slug: r13-e9-1-promote
mode: heavy
work_type: spec-slice
risk_class: high
version: 1
acceptance_criteria:
  - 'crates/id_effect_ai (moved by r13-t0-skeletons) reaches the integration bar: `publish` removed, entry removed from `PENDING_PROMOTION`, tier integration in its manifest; every 0.13 feature and optional dependency declared once in Cargo.toml (`openai`, `anthropic`, `ollama`, `cursor` (non-default, D6), `tools`, `embeddings`, `mcp`, `mcp-http`, `test-util`) so waves 2–3 never edit the manifest.'
  - 'Module layout fixed for later leaves: `chat`, `stream`, `vendors/{openai,anthropic,ollama}/{mod,chat,embed}.rs`, empty `tools/`, `embeddings/`, `mcp/` modules, `testing` (`ScriptedModel`: deterministic replies, tool calls and streams for tests, D8); ai.llm #1 (OpenAI + Anthropic chat) and #2 (streaming) and #7 (retries + spans + usage) kept and re-evidenced.'
  - 'Secure by default (ADR 0013): every HTTP call goes through `id_effect_platform`''s client, so the 0.11 SSRF policy, timeouts and body caps apply (R6, R4); API keys are `id_effect_config::Secret` (R2); prompts and completions are never logged or put in span attributes by default, `dangerous_log_prompts()` records itself via `posture::record_dangerous` (R3, R10); `max_tokens` has a finite default and a stream has a byte and event cap (R4).'
  - 'Tests `ai_api_key_redacted_in_debug`, `ai_prompts_not_logged_by_default`, `dangerous_log_prompts_is_recorded`, `ai_base_url_obeys_ssrf_policy`, `r4_ai_max_tokens_finite_by_default`, `r4_ai_stream_over_cap_closed`, `scripted_model_replays_in_order`, plus the existing openai/anthropic wiremock, streaming and cursor fixture tests; `stream::decode_sse_event` is public for the fuzz target.'
  - 'Book `part6/ch35-00-ai.md` rewritten from labs wording to the published crate (chat, streaming, testing with `ScriptedModel`, the safety defaults); snippets compiled by book-snippets.'
  - 'Tier: integration (ADR 0010 Amendment 9; moves to platform in r13-w6-pipeline). Coverage: `cargo llvm-cov nextest -p id_effect_ai --all-features --summary-only --fail-under-regions 96`.'
  - 'Gate: `bash scripts/tier-conformance.sh id_effect_ai`; `cargo nextest run -p id_effect_ai --all-features`; `cargo test -p id_effect_ai --doc`; `bash scripts/check-dangerous.sh`'
  - 'Owns: `crates/id_effect_ai/**` (every file in wave 1), `crates/id_effect/book/src/part6/ch35-00-ai.md`'
---

# r13-e9-1-promote

Leaf of `.maestro/specs/releases/0.13.0.md`, section "E9.1 — AI". The release spec's row is binding;
this file exists so Maestro can track the leaf as its own task.

Wave 1 (critical for the AI branch). Blocked by: r13-t0-skeletons. Blocks r13-e9-1-tools and
r13-e9-1-embeddings.
