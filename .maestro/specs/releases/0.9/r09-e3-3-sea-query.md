---
title: 'E3.3 — Query builder + Repository: r09-e3-3-sea-query'
slug: r09-e3-3-sea-query
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - '`select/insert/update/delete` builders from sea-query rendered per backend dialect; values bridged to `SqlValue` (D3); `query_as::<T: FromRow>(raw_sql)` (#6).'
  - 'Gate: `cargo nextest run -p id_effect_sql --all-features`'
  - 'Owns: `crates/id_effect_sql/src/query/**`, feature `query`'
---

# r09-e3-3-sea-query

Leaf of `.maestro/specs/releases/0.9.0.md`, section "E3.3 — Query builder + Repository (L, wave 2, after E3.1a) · data.orm #1 #2 #5 #6 #8 (+ sec.authz#5, eco.tenancy#3 slack)". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.
