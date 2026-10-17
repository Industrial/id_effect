---
title: 'Ledger reference app: r10-w3-kpis'
slug: r10-w3-kpis
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'K2 ≤ 8 s, K6 recorded, K3 re-measured with the job (§8 definition), K1 on an idle machine (0.9 follow-up).'
  - 'Gate: `DX_KPIS_RELEASE=0.10 bash scripts/dx-kpis.sh`'
  - 'Owns: `docs/benchmarks/dx-kpis-0.10.json`, `docs/benchmarks/k3/invoice.patch` (adds the job)'
---

# r10-w3-kpis

Leaf of `.maestro/specs/releases/0.10.0.md`, section "Ledger reference app (M, wave 3) — the 0.10 KPI". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.
