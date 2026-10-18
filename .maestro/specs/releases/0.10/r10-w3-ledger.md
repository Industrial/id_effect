---
title: 'Ledger reference app: r10-w3-ledger'
slug: r10-w3-ledger
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'See below. Tier flips to `platform` for jobs, messaging, mail, console, storage.'
  - 'Gate: `<docker> -p ledger`; `bash scripts/tier-conformance.sh` (all PASS)'
  - 'Owns: `examples/reference/ledger/**`, `.github/workflows/ci.yml` (`reference-ledger` job services: Mailpit, Redis, S3 image), tier lines of the new crates'
---

# r10-w3-ledger

Leaf of `.maestro/specs/releases/0.10.0.md`, section "Ledger reference app (M, wave 3) — the 0.10 KPI". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.
