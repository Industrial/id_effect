---
title: 'Ledger reference app: r10-w3-kit-template'
slug: r10-w3-kit-template
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'Kit features `jobs`, `jobs-cron`, `messaging`, `redis-streams`, `mail`, `console`, `storage`, `events`, `auth-accounts`; `full` updated; the service template gains opt-in (off by default) jobs + console wiring so K2 stays lean; `id_effect_kit` tier → `platform`.'
  - 'Gate: kit tests; CI template matrix; `bash scripts/tier-conformance.sh id_effect_kit`'
  - 'Owns: `crates/id_effect_kit/Cargo.toml` + `src/prelude.rs`, `crates/id_effect_tool/templates/service/**`'
---

# r10-w3-kit-template

Leaf of `.maestro/specs/releases/0.10.0.md`, section "Ledger reference app (M, wave 3) — the 0.10 KPI". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.
