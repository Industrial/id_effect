---
title: 'E9.3 — Admin editing: create/update/delete, relation pickers, bulk actions, audit: r13-e9-3-admin-edit'
slug: r13-e9-3-admin-edit
mode: heavy
work_type: spec-slice
risk_class: high
version: 1
acceptance_criteria:
  - 'web.admin #1 (write half): create, edit and delete forms from `Entity::FIELDS` (editable fields only); `Schema` validation errors bound to fields through the axum `ui` form helpers, translated when feature `i18n` is on; delete asks for confirmation; writes run through the repository so lifecycle hooks (r13-e3-3-hooks) apply.'
  - 'web.admin #4 (margin): belongs_to fields render a picker (search over the target entity, tenant-scoped), has_many and many_to_many render inline lists with add/remove for join-table relations.'
  - 'web.admin #5 (margin): bulk actions over selected rows (delete and registered custom actions), at most 100 ids per request.'
  - 'web.admin #6 (margin): every create, update, delete and bulk action is written to the 0.9 hash-chained audit log (feature `audit`) with principal, entity, id and changed fields.'
  - 'Secure by default (ADR 0013 R1, R4, R9): only declared-editable fields are bound (mass-assignment guard; others ignored and logged); form body ≤ 64 KiB; masked CSRF token required on every non-GET; bulk cap; each action checks its permission. Tests `admin_form_ignores_undeclared_fields`, `r4_admin_bulk_action_over_cap_rejected`, `r4_admin_form_over_cap_413`, `r9_admin_post_without_csrf_token_refused`, `admin_change_written_to_audit_log`, `admin_fk_picker_lists_only_tenant_rows`.'
  - 'Functional tests `admin_create_update_delete_round_trip`, `admin_validation_errors_rendered`, `admin_many_to_many_add_remove`, `admin_bulk_custom_action`; example `admin_site.rs` with `--self-test`; `forms::decode_form` public for the fuzz target `admin_form_body`; book `part6/ch30-08-admin.md` gains editing, relations, bulk and audit sections.'
  - 'Tier: integration. Coverage: `cargo llvm-cov nextest -p id_effect_admin --all-features --summary-only --fail-under-regions 96`.'
  - 'Gate: `cargo nextest run -p id_effect_admin --all-features`; `cargo run -p id_effect_admin --all-features --example admin_site -- --self-test`'
  - 'Owns: `crates/id_effect_admin/src/{edit.rs,forms.rs,bulk.rs,relations.rs,audit.rs}`, `crates/id_effect_admin/tests/{edit.rs,bulk.rs,relations.rs}`, `crates/id_effect_admin/examples/admin_site.rs`, the editing sections of `crates/id_effect/book/src/part6/ch30-08-admin.md`'
---

# r13-e9-3-admin-edit

Leaf of `.maestro/specs/releases/0.13.0.md`, section "E9.3 + E1.3 — Admin". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.

Wave 3, critical path. Blocked by: r13-e9-3-admin-core, r13-e3-3-relations, r13-e3-3-pagination,
r13-e3-3-hooks (and r13-e9-4-i18n-web for translated errors). Blocks r13-e1-3-generate-admin and
r13-e-fuzz-security.
