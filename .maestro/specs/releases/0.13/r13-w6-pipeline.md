---
title: 'Wave 6 — Pipeline reference app, core: multi-tenant intake, AI stages, MCP, publish hook: r13-w6-pipeline'
slug: r13-w6-pipeline
mode: heavy
work_type: spec-slice
risk_class: high
version: 1
acceptance_criteria:
  - 'D1 stages 1–4, 6 and 7 in examples/reference/pipeline, built only through id_effect_kit (features sql-pg, repository, audit, ai, embeddings, mcp, tenancy, rpc, jobs as needed): ingest by RPC mutation or upload; extraction by structured output into a typed `Extraction`; classification by a bounded tool loop (`lookup_source`, `list_labels`); embeddings flag near-duplicates within the tenant (bounded candidate set); an `after_update` hook writes `DocumentPublished` to the outbox; an RPC subscription shows live status; an MCP server exposes `search_documents` and `get_document`, an MCP client enriches from a fixture MCP server.'
  - 'Data model `Source` 1:N `Document` N:M `Label` (through `document_labels`), `Document` 1:N `Extraction`, all tenant-scoped, read with eager loading and keyset pagination; tenants `acme` (main database) and `globex` (its own database through `TenantRouter`).'
  - 'D8: deterministic AI only: `ScriptedModel` plus the wiremock OpenAI/Anthropic fixtures; no API key, no network to vendors.'
  - 'CI job `reference-pipeline` in .github/workflows/ci.yml (Postgres via id_effect_test containers, two databases); id_effect_ai and id_effect_i18n tier lines move to `platform` (check P; i18n is used for the extraction labels and the MCP tool descriptions in en/de).'
  - 'Tests `pipeline_ingest_to_publish_end_to_end`, `pipeline_tenant_isolation_across_relations`, `pipeline_tenant_db_routing`, `pipeline_eager_loading_no_n_plus_one`, `pipeline_keyset_pagination_stable_under_inserts`, `pipeline_ai_tool_loop_bounded`, `pipeline_prompt_injection_cannot_call_unlisted_tool` (a hostile document asks the model to call an unregistered tool), `pipeline_mcp_tools_round_trip`, `pipeline_mcp_refuses_foreign_origin`, `pipeline_publish_hook_writes_outbox`.'
  - 'Tier: example (publish = false). Secure by default: every route declared (`.public()` only for health), MCP guarded and Origin-checked, prompts not logged, tenant from the principal; prod posture checked in r13-w7-pipeline-backoffice once the back office exists.'
  - 'Gate: `ID_EFFECT_REQUIRE_PG=1 cargo nextest run -p pipeline`; CI job `reference-pipeline`; `bash scripts/tier-conformance.sh`'
  - 'Owns: `examples/reference/pipeline/{Cargo.toml,README.md,moon.yml,compose.yaml}`, `examples/reference/pipeline/migrations/**`, `examples/reference/pipeline/src/{main.rs,lib.rs,domain.rs,stages.rs,ai.rs,tenancy.rs,mcp.rs,rpc.rs}`, `examples/reference/pipeline/src/stages/**`, `examples/reference/pipeline/tests/{core.rs,tenancy.rs,ai.rs,mcp.rs}`, `examples/reference/pipeline/fixtures/**`, `.github/workflows/ci.yml` (job `reference-pipeline`), the tier lines of `crates/id_effect_ai/Cargo.toml` and `crates/id_effect_i18n/Cargo.toml`'
---

# r13-w6-pipeline

Leaf of `.maestro/specs/releases/0.13.0.md`, section "Proof" (the §6 exit KPI, first half). The release
spec's row is binding; this file exists so Maestro can track the leaf as its own task.

Wave 6, critical path. Blocked by: r13-w5-kit-template. Runs beside r13-f-relay-host (which rebases its
ci.yml steps after this leaf). Blocks r13-w7-pipeline-backoffice. Never cut.
