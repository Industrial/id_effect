---
title: 'E9.1 — Typed tool calling, structured output and a bounded agent loop: r13-e9-1-tools'
slug: r13-e9-1-tools
mode: heavy
work_type: spec-slice
risk_class: high
version: 1
acceptance_criteria:
  - 'ai.llm #3: `Tool<I, O>` with `I, O: HasSchema`; parameters exported as JSON Schema through crates/id_effect/src/schema/json_schema_export.rs; OpenAI and Anthropic tool-call formats both supported; `ToolSet` groups tools for a call.'
  - 'ai.llm #4 (margin): structured output decoded through `Schema` (refinements run); a decode failure is a typed error with at most one bounded repair retry.'
  - 'ai.llm #8 (margin): `ToolLoop` runs model → tool → model with the D9 budgets (8 steps, 32 tool calls, 60 s, tool output 64 KiB; each configurable, none infinite).'
  - 'Secure by default (D9, ADR 0013 R1 R4): only tools registered on the call''s `ToolSet` run (an unknown name is a typed refusal returned to the model, never executed); arguments are decoded through the tool''s Schema before the handler; tools declared `effects = write` need `ToolPolicy::allow_writes`; budgets enforced. Tests `ai_unregistered_tool_call_refused`, `ai_tool_arguments_schema_decoded`, `ai_write_tool_denied_without_policy`, `ai_tool_loop_step_budget_enforced`.'
  - 'Functional tests `tool_schema_matches_has_schema`, `structured_output_runs_refinements`, `structured_output_repair_retry_bounded`, `tool_loop_round_trip_openai_and_anthropic` (wiremock), `tool_loop_with_scripted_model`; `tools::decode_arguments` is public for the fuzz target.'
  - 'Book `part6/ch35-01-tools-structured-output.md` (tools, structured output, the loop, the safety rule and its residual risk).'
  - 'Tier: integration. Coverage: `cargo llvm-cov nextest -p id_effect_ai --all-features --summary-only --fail-under-regions 96`; new code ≥ 96 % regions.'
  - 'Gate: `cargo nextest run -p id_effect_ai --features tools,openai,anthropic,test-util`'
  - 'Owns: `crates/id_effect_ai/src/{tools/**,structured.rs,agent.rs}`, `crates/id_effect_ai/src/vendors/{openai,anthropic}/chat.rs` (tool-call fields), `crates/id_effect_ai/tests/{tools.rs,structured.rs,agent_loop.rs}`, `crates/id_effect/book/src/part6/ch35-01-tools-structured-output.md`'
---

# r13-e9-1-tools

Leaf of `.maestro/specs/releases/0.13.0.md`, section "E9.1 — AI". The release spec's row is binding;
this file exists so Maestro can track the leaf as its own task.

Wave 2. Blocked by: r13-e9-1-promote. Runs beside r13-e9-1-embeddings (disjoint: `embed.rs` vs
`chat.rs`). Blocks r13-e9-1-mcp and r13-e-fuzz-security.
