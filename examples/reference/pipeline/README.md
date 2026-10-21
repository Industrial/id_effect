# The Pipeline reference app

The 0.13 reference app (release spec 0.13.0, decision D1): a multi-tenant document-intake pipeline
and its back office, built only through `id_effect_kit`. It is tier `example` and `publish = false`.
The name is the app's; `id_effect_resilience::Pipeline` is an unrelated policy type.

1. **Ingest** — documents arrive by RPC mutation or upload for a tenant; one job per stage.
2. **Extract** — an AI model turns text into a typed `Extraction` (structured output).
3. **Classify** — a bounded tool loop calls `lookup_source` and `list_labels` and attaches labels.
4. **Embed** — embeddings flag near-duplicates within the tenant.
5. **Review** — reviewers work in the generated admin and an htmx review queue, in English or German.
6. **Publish** — an `after_update` hook writes `DocumentPublished` to the outbox.
7. **Agents** — an MCP server exposes `search_documents` / `get_document` to outside agents.

0.13 builds it in steps; this first step fixes the stage model.

```rust
use pipeline::{Stage, stages};

let all = stages().block_on().into_result().unwrap();
assert_eq!(all.len(), 6);
assert_eq!(Stage::Ingest.next(), Some(Stage::Extract));
```

## Run

```bash
cargo run -p pipeline --example 010_stages
cargo nextest run -p pipeline
```
