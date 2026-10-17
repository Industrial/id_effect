---
title: 'Ledger reference app: r09-w3-ledger'
slug: r09-w3-ledger
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'See below.'
  - 'Gate: `ID_EFFECT_REQUIRE_DOCKER=1 cargo nextest run -p ledger`; `bash scripts/tier-conformance.sh` (all PASS)'
  - 'Owns: `examples/reference/ledger/**`, root `Cargo.toml` member, `.github/workflows/ci.yml` (`reference-ledger` job), tier flips to `platform` in the new crates'' manifests'
---

# r09-w3-ledger

Leaf of `.maestro/specs/releases/0.9.0.md`, section "Ledger reference app (M, wave 3) — the 0.9 KPI". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.
