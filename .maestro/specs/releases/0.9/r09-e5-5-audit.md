---
title: 'E5.5 — Audit log + tenancy pattern: r09-e5-5-audit'
slug: r09-e5-5-audit
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - '`AuditLog` Layer appends `{actor, tenant, action, entity, before, after, at}` inside the caller''s transaction; each record stores `sha256(prev_hash ‖ record)`; `verify_chain()` reports the first broken link. Tests `tampered_row_breaks_chain`, `audit_rolls_back_with_tx`.'
  - 'Gate: pg nextest `--features audit`'
  - 'Owns: `crates/id_effect_sql/src/audit.rs` (feature `audit`), its migration'
---

# r09-e5-5-audit

Leaf of `.maestro/specs/releases/0.9.0.md`, section "E5.5 — Audit log + tenancy pattern (M, wave 2, after E3.1a) · eco.tenancy #1 #5 #6". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.
