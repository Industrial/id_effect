---
title: 'E3.3 part 2 — Repository relations with eager loading and entity field metadata: r13-e3-3-relations'
slug: r13-e3-3-relations
mode: heavy
work_type: spec-slice
risk_class: high
version: 1
acceptance_criteria:
  - 'crates/id_effect_sql/src/repository.rs becomes the module tree `repository/{mod,relations,page,hooks}.rs` (`page.rs` and `hooks.rs` created empty for wave 2); every existing public path is kept by `pub use`, so the Ledger and `tests/repository.rs` compile unchanged.'
  - 'data.orm #3: `#[entity(belongs_to(...))]`, `has_many(...)`, `many_to_many(..., through = "...")` in crates/id_effect_proc_macro/src/repository.rs (this leaf declares every 0.13 entity attribute once, including `editable` and `label` used by the admin); `Repository::with(Rel)` eager-loads in one `IN (...)` query per relation per level (no N+1), nested at most 3 levels; works on Postgres, MySQL and SQLite through sea-query.'
  - '`Entity::FIELDS: &[FieldMeta]` (field name, column, Schema type name, nullable, editable, label key, relation target) and `Entity::RELATIONS`, both with defaults so hand-written `impl Entity` compiles (no semver break); the design of FieldMeta is written in the leaf evidence before admin-core starts.'
  - 'Secure by default (ADR 0013 R1): eager loads and joins re-apply the child entity''s tenant and soft-delete filters; a relation can never return a row of another tenant. Tests `m19_eager_loaded_children_scoped_to_tenant`, `relation_join_cannot_cross_tenant`.'
  - 'Functional tests `eager_load_issues_one_query_per_relation` (query-count probe on the executor), `many_to_many_through_join_table`, `nested_eager_load_three_levels`, `entity_fields_metadata_from_derive`, `manual_entity_impl_still_compiles`; SQLite in the default run and Postgres under `<pg>`.'
  - 'Book `part6/ch28-06-relations.md`; `ch28-03-repositories.md` links it.'
  - 'Tier: integration (id_effect_sql, id_effect_proc_macro). Coverage: `cargo llvm-cov nextest -p id_effect_sql --features repository,sqlite --summary-only --fail-under-regions 96` (proc-macro entry stubs stay excluded as in scripts/llvm-cov-nextest.sh).'
  - 'Gate: `cargo nextest run -p id_effect_sql --features repository,sqlite`; `ID_EFFECT_REQUIRE_PG=1 cargo nextest run -p id_effect_sql --features repository,pg --test repository_relations_pg`; `cargo nextest run -p id_effect_proc_macro`; `ID_EFFECT_REQUIRE_DOCKER=1 cargo nextest run -p ledger` (imports unchanged)'
  - 'Owns: `crates/id_effect_sql/src/repository.rs` → `crates/id_effect_sql/src/repository/{mod,relations}.rs` (plus empty `page.rs`, `hooks.rs`), `crates/id_effect_proc_macro/src/repository.rs`, `crates/id_effect_sql/tests/{relations.rs,repository_relations_pg.rs}`, `crates/id_effect/book/src/part6/ch28-06-relations.md`'
---

# r13-e3-3-relations

Leaf of `.maestro/specs/releases/0.13.0.md`, section "E3.3 part 2". The release spec's row is binding;
this file exists so Maestro can track the leaf as its own task.

Wave 1. Blocked by: r13-t0-skeletons. Blocks r13-e3-3-pagination, r13-e3-3-hooks, r13-e5-5-tenancy
(sql `lib.rs` line), r13-e9-3-admin-edit (FieldMeta and relations).
