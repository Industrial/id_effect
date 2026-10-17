---
title: 'Ledger reference app: r09-w3-kit-template'
slug: r09-w3-kit-template
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'Kit features `sql-sqlite`, `sql-mysql`, `migrate`, `openapi`, `auth`, `authz`, `test` (+`redis`); `full` updated; service template replaces the `include_str!` migration (`db.rs.template:10,26`) with `migrate::run`.'
  - 'Gate: kit tests; CI template matrix (`ci.yml` template job)'
  - 'Owns: `crates/id_effect_kit/Cargo.toml` + `src/prelude.rs`, `crates/id_effect_tool/templates/service/**`'
---

# r09-w3-kit-template

Leaf of `.maestro/specs/releases/0.9.0.md`, section "Ledger reference app (M, wave 3) — the 0.9 KPI". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.
