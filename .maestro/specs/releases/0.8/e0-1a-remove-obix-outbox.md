---
title: "0.8 E0.1a — remove the labs obix outbox adapter (relicense prerequisite)"
slug: r08-e0-1a-remove-obix-outbox
mode: light
work_type: maintenance
risk_class: medium
version: 1
acceptance_criteria:
  - 'Owner decision 2026-10-16: instead of seeking consent for commit 8eb9b05 (#79, Justin Carter), remove the code that still carries it. labs/id_effect_jobs/src/obix_outbox.rs (ObixOutbox, JobsOutboxEvent) is deleted with its lib.rs mod/re-exports, its tests in tests/pg_integration.rs, and its use in examples (010/020); the obix feature keeps ObixInbox only (or is reshaped if the inbox depends on removed items); README, moon.yml and the book (part6 ch31) no longer mention the obix outbox adapter; the 0.10 jobs battery (PLAN-OF-PLANS E7) is noted as the place an outbox relay returns.'
  - 'git blame -C -C -M over every tracked file in the working tree attributes 0 lines to Justin Carter (scripted check over labs/ and crates/id_effect/book/).'
  - 'labs: cargo check --manifest-path labs/Cargo.toml --workspace --all-targets and with --features obix for id_effect_jobs; cargo test for id_effect_jobs (non-pg tests); clippy -D warnings for id_effect_jobs; book still builds (moon run :book).'
non_goals:
  - Re-implementing an outbox relay (0.10).
---

# 0.8 E0.1a — remove the labs obix outbox adapter

Unblocks E0.1 (MIT relicense) without third-party consent. Release spec `.maestro/specs/releases/0.8.0.md`.
