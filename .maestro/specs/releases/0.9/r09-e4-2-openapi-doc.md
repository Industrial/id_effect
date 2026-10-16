---
title: 'E4.2 — OpenAPI 3.1 + docs UI: r09-e4-2-openapi-doc'
slug: r09-e4-2-openapi-doc
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - '`OpenApi::from_router(&router)` builds a 3.1 document whose component schemas are `to_json_schema` of the same Schemas the extractors decode with (#4); `securitySchemes` from declared auth (bearer JWT, cookie session, OIDC) (#7). Test validates the output against the OpenAPI 3.1 meta-schema and asserts schema identity with `json_schema_for::<T>()`.'
  - 'Gate: `cargo nextest run -p id_effect_axum --features openapi`'
  - 'Owns: `crates/id_effect_axum/src/openapi/**` (feature `openapi`)'
---

# r09-e4-2-openapi-doc

Leaf of `.maestro/specs/releases/0.9.0.md`, section "E4.2 — OpenAPI 3.1 + docs UI (M, wave 2, after E4.1) · data.jsonschema #2 #3 #4 #7, web.server #8". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.
