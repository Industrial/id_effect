---
title: 'Ledger reference app: r10-w3-level-audit'
slug: r10-w3-level-audit
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'For each of the 14 rows + sec.authn: every planned check id maps to an existing path at HEAD and the row has book + example + integration test; gaps become tasks.'
  - 'Gate: `python3 -I .tmp/plan10/gaps.py v` compared against the evidence map'
  - 'Owns: none (read-only)'
---

# r10-w3-level-audit

Leaf of `.maestro/specs/releases/0.10.0.md`, section "Ledger reference app (M, wave 3) — the 0.10 KPI". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.
