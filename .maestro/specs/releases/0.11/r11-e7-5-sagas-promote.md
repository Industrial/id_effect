---
title: 'E7.5 — Durable sagas, docs and labs removal: r11-e7-5-sagas-promote'
slug: r11-e7-5-sagas-promote
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'fault.workflows#3 on the published crate: ctx.saga() registers compensations as journaled activities; on failure they run in reverse order, at least once, and survive a restart (the id_effect_fsm saga at crates/id_effect_fsm/src/saga.rs:66 stays the in-memory variant).'
  - 'Level labs → documented: book part6/ch32-00-workflow-cluster.md rewritten for id_effect_workflow (labs banner removed), part5/ch19-04-workflow-bridge.md updated; runnable examples 010_order_saga, 020_durable_timer, 030_signal_approval (DATABASE_URL); README with a rust fence; integration test `saga_compensates_in_reverse_after_restart`.'
  - 'labs/id_effect_workflow deleted with its duroxide, duroxide-pg and rusqlite dependencies and the NetworkJournalStub (labs/id_effect_workflow/src/journal.rs:131, QUALITY_BAR check S); labs/Cargo.toml member removed.'
  - 'The leaf evidence maps fault.workflows #1 #2 #3 #4 #5 #7 to paths and tests at HEAD (6/8 = 75 at documented).'
  - 'Gate: `ID_EFFECT_REQUIRE_PG=1 cargo nextest run -p id_effect_workflow --all-features` (Postgres required); `cargo test --manifest-path labs/Cargo.toml --workspace`; `bash scripts/tier-conformance.sh id_effect_workflow`'
  - 'Owns: `crates/id_effect_workflow/{src/saga.rs,src/lib.rs,README.md,examples/**,tests/saga_pg.rs}`, `crates/id_effect/book/src/part6/ch32-00-workflow-cluster.md`, `crates/id_effect/book/src/part5/ch19-04-workflow-bridge.md`, `labs/id_effect_workflow/**` (deleted), `labs/Cargo.toml`'
---

# r11-e7-5-sagas-promote

Leaf of `.maestro/specs/releases/0.11.0.md`, section "E7.5 — Workflows". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.

Wave 3. Blocked by: r11-e7-5-activities-timers. r11-w3-ledger then uses the crate for account
closure (promotion to tier platform).
