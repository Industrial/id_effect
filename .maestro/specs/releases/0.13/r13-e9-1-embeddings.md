---
title: 'E9.1 — Embeddings across providers (OpenAI, Ollama): r13-e9-1-embeddings'
slug: r13-e9-1-embeddings
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'ai.llm #5: `Embedder` trait with OpenAI and Ollama adapters returning the same `Embedding` shape; batch size cap with automatic splitting; dimension check (mismatch is a typed error); cosine and dot-product helpers; retries, spans and usage telemetry as for chat (#7).'
  - 'Secure by default: the platform client (SSRF, timeouts, caps; R6 R4) and `Secret` keys (R2) from r13-e9-1-promote apply; input text is never logged (R3).'
  - 'Tests `embeddings_openai_and_ollama_same_shape` (wiremock), `embedding_batch_over_cap_split`, `embedding_dimension_mismatch_is_typed_error`, `embedding_input_not_logged`; `ScriptedModel` gains deterministic embeddings for downstream tests.'
  - 'Book `part6/ch35-02-embeddings.md` (no vector database: in-app similarity over a bounded set, the Pipeline pattern).'
  - 'Tier: integration. Coverage: `cargo llvm-cov nextest -p id_effect_ai --all-features --summary-only --fail-under-regions 96`.'
  - 'Gate: `cargo nextest run -p id_effect_ai --features embeddings,openai,ollama,test-util`'
  - 'Owns: `crates/id_effect_ai/src/embeddings/**`, `crates/id_effect_ai/src/vendors/{openai,ollama}/embed.rs`, `crates/id_effect_ai/tests/embeddings.rs`, `crates/id_effect/book/src/part6/ch35-02-embeddings.md`'
---

# r13-e9-1-embeddings

Leaf of `.maestro/specs/releases/0.13.0.md`, section "E9.1 — AI". The release spec's row is binding;
this file exists so Maestro can track the leaf as its own task.

Wave 2. Blocked by: r13-e9-1-promote. Runs beside r13-e9-1-tools. `ScriptedModel` lives in
`src/testing.rs` (promote); this leaf adds the embedding method there as an append-only hunk, rebased
after tools if both touch it.
