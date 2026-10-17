---
title: 'Ledger reference app: r09-w3-level-audit'
slug: r09-w3-level-audit
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'For each of the 15 rows: every planned check id maps to an existing path at HEAD and the row has book + example + integration test; list gaps as tasks.'
  - 'Gate: `python3 -I .tmp/plan09/gaps.py v` compared against the evidence map'
  - 'Owns: none (read-only)'
---

# r09-w3-level-audit

Leaf of `.maestro/specs/releases/0.9.0.md`, section "Ledger reference app (M, wave 3) — the 0.9 KPI". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.
