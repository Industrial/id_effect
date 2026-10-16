---
title: 'E3.3 — Query builder + Repository: r09-e3-3-repository'
slug: r09-e3-3-repository
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - '`#[derive(Entity)]` (table/columns, #1) and `Repository<T>` CRUD; `#[entity(version)]` column → update fails with `SqlError::StaleVersion` (#5); `#[entity(scope = …)]` global filters (soft delete, tenant/owner) applied to every generated query (#8). Tests `stale_version_is_rejected`, `global_filter_hides_soft_deleted`, `tenant_filter_scopes_every_query`.'
  - 'Gate: pg + sqlite nextest'
  - 'Owns: `crates/id_effect_proc_macro/src/repository.rs` + derive line, `crates/id_effect_sql/src/repository.rs`'
---

# r09-e3-3-repository

Leaf of `.maestro/specs/releases/0.9.0.md`, section "E3.3 — Query builder + Repository (L, wave 2, after E3.1a) · data.orm #1 #2 #5 #6 #8 (+ sec.authz#5, eco.tenancy#3 slack)". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.
