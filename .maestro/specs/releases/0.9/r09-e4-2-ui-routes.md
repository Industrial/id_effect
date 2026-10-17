---
title: 'E4.2 — OpenAPI 3.1 + docs UI: r09-e4-2-ui-routes'
slug: r09-e4-2-ui-routes
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - '`/openapi.json`, `/docs` (Scalar, D11), `/_routes` route table (method, path, handler name, schemas) behind a feature and an opt-in mount (web.server#8). Book `ch29-02-openapi.md`, example `085_openapi.rs`, integration test over a socket.'
  - 'Gate: same + book-snippets; level must reach `documented` (0.8 miss cause)'
  - 'Owns: `crates/id_effect_axum/src/openapi/ui.rs`, `src/route/introspect.rs`'
---

# r09-e4-2-ui-routes

Leaf of `.maestro/specs/releases/0.9.0.md`, section "E4.2 — OpenAPI 3.1 + docs UI (M, wave 2, after E4.1) · data.jsonschema #2 #3 #4 #7, web.server #8". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.
