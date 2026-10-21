---
title: 'Wave 8 — Security and DX KPIs for 0.13: r13-w8-kpis'
slug: r13-w8-kpis
mode: heavy
work_type: spec-slice
risk_class: low
version: 1
acceptance_criteria:
  - 'docs/security/kpis-0.13.json via scripts/security-kpis.sh (0.13 fields added): open highs 0; fuzz targets ≥ 22 with ≥ 5 new and no open crash; compile-fail cases include r1_admin_site_without_guard and r1_mcp_endpoint_without_access_decision; secure-stack delta ≤ 5 µs and unary RPC delta ≤ 3 µs from docs/benchmarks/v0.13.json; every Security-table test green (from surface_013.rs).'
  - 'docs/benchmarks/dx-kpis-0.13.json: K1 ≤ 180 s, K2 ≤ 8 s, K4 ≥ 17/20, K5 ≥ 80 %, K6 within the 0.10 budget, plus the cold-build delta of kit features ai, admin, ui, i18n each measured separately on the `service` template.'
  - 'Workspace coverage gate `scripts/llvm-cov-nextest.sh` ≥ 96 % regions with id_effect_ai, id_effect_admin, id_effect_i18n included; any new exclusion has a justification line in the script.'
  - 'Tier: docs and scripts. Secure by default: the KPI file is the release''s security evidence.'
  - 'Gate: `bash scripts/security-kpis.sh --self-test && bash scripts/security-kpis.sh 0.13.0`; `DX_KPIS_RELEASE=0.13 bash scripts/dx-kpis.sh`; `bash scripts/llvm-cov-nextest.sh`'
  - 'Owns: `docs/security/kpis-0.13.json`, `scripts/security-kpis.sh` (0.13 fields), `docs/benchmarks/dx-kpis-0.13.json`'
---

# r13-w8-kpis

Leaf of `.maestro/specs/releases/0.13.0.md`, section "Proof". The release spec's row is binding; this
file exists so Maestro can track the leaf as its own task.

Wave 8 (last). Blocked by: r13-w7-pipeline-backoffice, r13-e-benches, r13-e-fuzz-security. Idle machine;
`DX_KPIS_DIR` and `TMPDIR` under `.tmp/` or `/data` (0.12 F61).
