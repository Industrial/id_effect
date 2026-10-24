# The Pipeline reference app

The 0.13 reference app (release spec 0.13.0, decision D1; the §6 exit KPI): a multi-tenant
document-intake pipeline and its back office, built only through `id_effect_kit`. It is tier
`example` and `publish = false`. The name is the app's; `id_effect_resilience::Pipeline` is an
unrelated policy type, so the docs always say "the Pipeline reference app".

```rust
use pipeline::{Stage, stages};

let all = stages().block_on().into_result().unwrap();
assert_eq!(all.len(), 6);
assert_eq!(Stage::Ingest.next(), Some(Stage::Extract));
```

## What it does

| Stage | How | Battery (kit feature) |
|---|---|---|
| 1. Ingest | `Intake.ingest` (RPC mutation) or `POST /documents/upload` (the raw text kept in object storage); an `ExtractJob` is queued | `rpc`, `storage`, `jobs` |
| 2. Extract | structured output into a typed `Extracted` (decoded through its `Schema`, one repair retry, then the app's rules); the organization is enriched from an outside MCP tool server | `ai`, `mcp` (client) |
| 3. Classify | a bounded tool loop (4 steps, 8 tool calls, 30 s) with exactly two read-only tools of the tenant, `lookup_source` and `list_labels`; only the tenant's own labels attach | `ai` (tools, agent loop) |
| 4. Embed | the document's embedding against at most 200 earlier embeddings of the same tenant; the closest at cosine ≥ 0.97 is the duplicate | `embeddings` |
| 5. Review | the generated admin and the htmx review queue (r13-w7-pipeline-backoffice) | `admin`, `ui`, `i18n` |
| 6. Publish | `Intake.publish`; the document's `after_update` hook stages `DocumentPublished` in the outbox **in the update's transaction**; `Intake.status` (a subscription, as server-sent events) shows every change live | `repository` (hooks), `messaging`, `rpc-ws` |
| 7. Agents | one MCP server per tenant at `/mcp/<tenant>`: `search_documents`, `get_document` (described in the tenant's locale) | `mcp` |

**Data model.** `Source` 1:N `Document` N:M `Label` (through `document_labels`), `Document` 1:N
`Extraction`, all tenant-scoped (`#[entity(tenant = "tenant")]`), read with eager loading (one
`IN` query per relation, never one per row) and keyset pages (cursors stay stable while documents
arrive).

**Tenants.** `acme` (English) shares the main database with the job queue; `globex` (German) has
a database of its own, reached through a `TenantRouter`. The tenant of every request is the
`tenant` claim of its bearer token (the kit's `TenancyLayer` sets both the authorization tenant
and the SQL tenant from it); a tenant without a database has no connection at all.

## Secure by default

- Every route declares its access: `/health` is the only public route; `/rpc` procedures each
  carry a guard (`editor` ingests, `reviewer` publishes, both read; a tenant claim is required);
  the upload route needs `editor`.
- Each MCP endpoint is guarded (an `agent` of that tenant), refuses browser origins outside
  `PIPELINE_MCP_ORIGINS` (none by default) and messages over 1 MiB, and exposes read-only tools.
- The model can call only the two registered tools; a tool a document talks it into calling is
  refused and fed back, never run (`pipeline_prompt_injection_cannot_call_unlisted_tool`). Every
  loop is bounded; prompts and completions are never logged.
- No live vendor call in CI (D8): `ScriptedModel` and the recorded OpenAI / Anthropic fixtures
  (`fixtures/*.json`) under wiremock.

## Run

```bash
# The tests start PostgreSQL in their own container (Docker required with ID_EFFECT_REQUIRE_PG=1).
ID_EFFECT_REQUIRE_PG=1 cargo nextest run -p pipeline

# A local service: PostgreSQL from compose.yaml, then the binary (configuration in src/main.rs).
docker compose -f examples/reference/pipeline/compose.yaml up -d
PIPELINE_DATABASE_URL=postgres://pipeline:pipeline@127.0.0.1:55432/pipeline \
PIPELINE_TENANT_DATABASE_GLOBEX=postgres://pipeline:pipeline@127.0.0.1:55432/globex \
PIPELINE_TOKEN_SECRET=change-me-to-at-least-32-random-bytes \
PIPELINE_TOKEN_ISSUER=https://id.example.com \
OPENAI_API_KEY=... \
  cargo run -p pipeline
```

| Test | Shows |
|---|---|
| `pipeline_ingest_to_publish_end_to_end` | RPC ingest, upload, the three stage jobs, MCP enrichment, the live status subscription, publish, the outbox row |
| `pipeline_publish_hook_writes_outbox` | the hook writes to the tenant's own database, atomically with the update |
| `pipeline_tenant_isolation_across_relations` | stray rows of another tenant never load through relations |
| `pipeline_tenant_db_routing` | `globex` rows in `globex`'s database; an unknown tenant has no connection |
| `pipeline_eager_loading_no_n_plus_one` | four queries for one document or six |
| `pipeline_keyset_pagination_stable_under_inserts` | every document once, in order, while documents arrive |
| `pipeline_ai_tool_loop_bounded` | a model that never stops calling tools is stopped at the budget |
| `pipeline_prompt_injection_cannot_call_unlisted_tool` | a hostile document cannot reach an unregistered tool |
| `pipeline_mcp_tools_round_trip` | the MCP tools over Streamable HTTP, in English and German |
| `pipeline_mcp_refuses_foreign_origin` | DNS-rebinding defence on the MCP endpoints |
