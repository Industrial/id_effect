---
title: 'E12.4 — OpenAPI 3.1 and AsyncAPI output from the contract: r12-e12-4-openapi-asyncapi'
slug: r12-e12-4-openapi-asyncapi
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'web.rpc#7: id_effect_rpc::openapi emits OpenAPI 3.1 from Service::contract() for queries and mutations (GET and POST forms, the x-rpc header, idempotency key, error unions as oneOf with a _tag discriminator, the batch endpoint); it replaces the labs 3.0 emitter (crates/id_effect_rpc/src/openapi.rs).'
  - 'id_effect_rpc::asyncapi emits AsyncAPI 3.0 for server streams, client streams, two-way streams, subscriptions and channels, with WebSocket and SSE bindings and the cursor field documented.'
  - 'Both documents validate against their meta-schemas (jsonschema, no remote resolution); /rpc/openapi.json and /rpc/asyncapi.json are closed in the prod profile unless guarded (ADR 0013 R7).'
  - 'Tests `openapi_31_validates_against_meta_schema`, `openapi_error_union_is_one_of_with_discriminator`, `asyncapi_30_validates`, `asyncapi_lists_every_stream_and_channel`, `r7_rpc_docs_closed_in_prod`; book part6/ch33-10-contracts.md.'
  - 'Gate: `cargo nextest run -p id_effect_rpc --all-features --test openapi_emit --test asyncapi_emit`'
  - 'Owns: `crates/id_effect_rpc/src/{openapi.rs,asyncapi.rs}`, `crates/id_effect_rpc/tests/{openapi_emit.rs,asyncapi_emit.rs}`, `crates/id_effect_rpc/tests/fixtures/asyncapi-3.0.schema.json`, `crates/id_effect/book/src/part6/ch33-10-contracts.md`'
---

# r12-e12-4-openapi-asyncapi

Leaf of `.maestro/specs/releases/0.12.0.md`, section "E12.4 — Clients and contracts". The release
spec's row is binding; this file exists so Maestro can track the leaf as its own task.

Wave 4. Blocked by: r12-e12-1-macro (the contract). The OpenAPI 3.1 meta-schema already used by
`crates/id_effect_axum/tests/openapi.rs` is reused; the AsyncAPI schema is vendored as a test fixture.
