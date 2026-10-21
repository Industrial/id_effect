---
title: 'E3.3 part 2 — Lifecycle hooks on persist, inside the transaction, with an outbox adapter: r13-e3-3-hooks'
slug: r13-e3-3-hooks
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'data.orm #7 (D12): `EntityHooks` with `before_insert`, `after_insert`, `before_update`, `after_update`, `before_delete`, `after_delete`, each an Effect run inside the write''s transaction (the repository opens one when the caller has none); a failing hook rolls the write back; hooks are registered per repository, not globally.'
  - 'Feature `outbox` of id_effect_sql (declared by this leaf in crates/id_effect_sql/Cargo.toml, optional dependency on id_effect_messaging): an adapter that writes an event to the transactional outbox in the same transaction.'
  - 'Secure by default: hooks cannot bypass tenant scoping (they receive the bound repository); the existing audit log keeps its explicit append and can be wired as an `after_*` hook (documented).'
  - 'Tests `hooks_run_in_order_inside_transaction`, `hook_failure_rolls_back_write`, `before_hook_can_reject_with_typed_error`, `after_update_writes_outbox_event_atomically` (SQLite and `<pg>`).'
  - 'Book `part6/ch28-08-lifecycle-hooks.md`.'
  - 'Tier: integration. Coverage: `cargo llvm-cov nextest -p id_effect_sql --features repository,sqlite,outbox --summary-only --fail-under-regions 96`.'
  - 'Gate: `cargo nextest run -p id_effect_sql --features repository,sqlite,outbox --test hooks`; `ID_EFFECT_REQUIRE_PG=1 cargo nextest run -p id_effect_sql --features repository,pg,outbox --test hooks`'
  - 'Owns: `crates/id_effect_sql/src/repository/{hooks.rs,outbox.rs}`, `crates/id_effect_sql/Cargo.toml` (feature `outbox` and its optional dependency only), `crates/id_effect_sql/tests/hooks.rs`, `crates/id_effect/book/src/part6/ch28-08-lifecycle-hooks.md`'
---

# r13-e3-3-hooks

Leaf of `.maestro/specs/releases/0.13.0.md`, section "E3.3 part 2". The release spec's row is binding;
this file exists so Maestro can track the leaf as its own task.

Wave 2. Blocked by: r13-e3-3-relations. Runs beside r13-e3-3-pagination and r13-e5-5-tenancy (the only
wave-2 leaf that edits the sql manifest). Blocks r13-e9-3-admin-edit and r13-w5-kit-template.
