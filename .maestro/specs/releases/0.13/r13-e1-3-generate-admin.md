---
title: 'E1.3 — `id-effect generate admin <Entity>`: r13-e1-3-generate-admin'
slug: r13-e1-3-generate-admin
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'web.admin #8 (generator half): `GenerateCommand::Admin` in crates/id_effect_tool/src/generate.rs (`:100`): `id-effect generate admin <Entity> [--read-only] [--permissions <prefix>]` writes `src/admin/<entity>.rs` (an `EntityAdmin` registration with list columns, search and filters from the entity''s fields) and registers it in `src/admin/mod.rs`, idempotently, with the same insertion helpers `generate job` uses; templates under crates/id_effect_tool/templates/generate/admin/.'
  - 'Generated code mounts the site with `.guard(g)` and the permission prefix (never public); `--read-only` registers list/view only.'
  - 'Tests `generate_admin_golden_output` (golden files under tests/golden/generate/admin/, `UPDATE_GOLDEN=1` rewrites), `generate_admin_is_idempotent`, `generate_admin_unknown_entity_is_error`, `generated_admin_compiles_in_service_template` (generates a `service` project, adds an entity, runs `generate admin`, builds and runs its test; until r13-w5-kit-template adds kit feature `admin`, the test project depends on id_effect_admin by path, and the generated import is `id_effect_admin::` behind one template variable that kit-template switches to `id_effect_kit::admin::`).'
  - 'Book `part6/ch30-08-admin.md` section "Generating an admin".'
  - 'Secure by default (R1): generated permissions default to deny until granted; no flag generates a public admin.'
  - 'Tier: integration (id_effect_tool). Coverage: `cargo llvm-cov nextest -p id_effect_tool --all-features --summary-only --fail-under-regions 96`.'
  - 'Gate: `cargo nextest run -p id_effect_tool --test generate_admin` (the template smoke with admin runs in r13-w5-kit-template, which owns scripts/template-smoke.sh)'
  - 'Owns: `crates/id_effect_tool/src/generate.rs`, `crates/id_effect_tool/templates/generate/admin/**`, `crates/id_effect_tool/tests/{generate_admin.rs,golden/generate/admin/**}`, the "Generating an admin" section of `crates/id_effect/book/src/part6/ch30-08-admin.md`'
---

# r13-e1-3-generate-admin

Leaf of `.maestro/specs/releases/0.13.0.md`, section "E9.3 + E1.3 — Admin". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.

Wave 4, critical path. Blocked by: r13-e9-3-admin-edit. Blocks r13-w5-kit-template (template smoke)
and r13-w7-pipeline-backoffice. Runs beside r13-f-rpc-retire (different tool files: `src/rpc/**`).
