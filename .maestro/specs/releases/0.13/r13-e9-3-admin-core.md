---
title: 'E9.3 — id_effect_admin core: registration API, list and detail, search/filter/sort/paging, RBAC: r13-e9-3-admin-core'
slug: r13-e9-3-admin-core
mode: heavy
work_type: spec-slice
risk_class: high
version: 1
acceptance_criteria:
  - 'D2: crates/id_effect_admin (skeleton from r13-t0-skeletons) gets its full Cargo.toml (every 0.13 feature once: `i18n`, `audit`, `test-util`; deps id_effect_axum `ui`, id_effect_sql `repository`, id_effect_authz) and module layout, creating empty `edit.rs`, `forms.rs`, `bulk.rs`, `relations.rs`, `audit.rs` for wave 3.'
  - 'web.admin #8: `AdminSite::new().register::<E>(EntityAdmin::default().list(...).search(...).filter(...))` registration API; the site mounts only through `.guard(g)` (H8 typestate), never `.public()`.'
  - 'web.admin #1 (read half) and #2: list and detail pages generated from `Entity::FIELDS` (r13-e3-3-relations) with text search over declared columns, typed filters, sortable columns and pagination (offset `Page` now; keyset `find_after` when r13-e3-3-pagination has merged), rendered with axum `ui` maud components and htmx partial updates; labels through id_effect_i18n when feature `i18n` is on.'
  - 'web.admin #3: per-entity × action permissions `admin.<entity>.<list|view|create|update|delete|bulk>` checked through id_effect_authz before any query; default deny; repositories are tenant-bound from `CurrentTenant`.'
  - 'Secure by default (ADR 0013 R1, R7, R9): no public admin; every action authorized; maud escaping with no `PreEscaped` outside `ui::trusted_html` (grep test covers this crate); the posture probe "admin guarded" is added by r13-w5-kit-template. Tests `admin_denied_without_permission`, `admin_action_requires_entity_permission`, `admin_list_scoped_to_tenant`, `no_preescaped_outside_trusted_module`.'
  - 'Functional tests `admin_list_search_filter_sort_page`, `admin_detail_renders_fields`, `admin_registration_is_typed`; book `part6/ch30-08-admin.md` (registration, list, permissions).'
  - 'Tier: integration (new crate; moves to platform in r13-w7-pipeline-backoffice). Coverage: `cargo llvm-cov nextest -p id_effect_admin --all-features --summary-only --fail-under-regions 96`.'
  - 'Gate: `cargo nextest run -p id_effect_admin --all-features`; `bash scripts/tier-conformance.sh id_effect_admin`'
  - 'Owns: `crates/id_effect_admin/**` except `src/{edit,forms,bulk,relations,audit}.rs` contents (created empty here), `crates/id_effect/book/src/part6/ch30-08-admin.md`'
---

# r13-e9-3-admin-core

Leaf of `.maestro/specs/releases/0.13.0.md`, section "E9.3 + E1.3 — Admin". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.

Wave 2, critical path. Blocked by: r13-e9-5-views, r13-e3-3-relations (`Entity::FIELDS`),
r13-t0-skeletons. Blocks r13-e9-3-admin-edit. Named in the release spec as the leaf most likely to be
wrong: review its registration API and the use of `FieldMeta` before admin-edit starts.
