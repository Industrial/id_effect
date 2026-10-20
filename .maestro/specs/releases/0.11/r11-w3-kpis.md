---
title: 'Wave 3 — Security and DX KPIs: r11-w3-kpis'
slug: r11-w3-kpis
mode: heavy
work_type: spec-slice
risk_class: low
version: 1
acceptance_criteria:
  - 'docs/security/findings.json registers all 41 findings of the 2026-10-18 review (H1–H8, M1–M20, L1–L6, S1–S7) with severity, status (closed / accepted), the release and leaf that closed it, and the regression test or gate.'
  - 'scripts/security-kpis.sh (with --self-test) writes docs/security/kpis-0.11.json: open highs (0), OpenSSF Scorecard score (from the scorecard workflow result, ≥ 7.5), Best Practices status (Passing), SLSA level and the count of published crates with a verified attestation (all of them), fuzz targets in CI (≥ 8) and open crashes (0), compile-fail security cases (≥ 15), secure-stack overhead: secure_stack − bare bench ≤ 5 µs per request, final stack, ratio reported for information only (owner decision 2026-10-19; was ratio ≤ 1.15).'
  - 'DX KPIs: `DX_KPIS_RELEASE=0.11 bash scripts/dx-kpis.sh` writes docs/benchmarks/dx-kpis-0.11.json with K1 ≤ 180 s, K2 ≤ 8 s, K4 ≥ 17/20, K5 ≥ 80 %, K3 and K6 recorded.'
  - 'Gate: `bash scripts/security-kpis.sh --self-test && bash scripts/security-kpis.sh 0.11.0`; `DX_KPIS_RELEASE=0.11 bash scripts/dx-kpis.sh`'
  - 'Owns: `docs/security/{findings.json,kpis-0.11.json}`, `scripts/security-kpis.sh`, `docs/benchmarks/dx-kpis-0.11.json`'
---

# r11-w3-kpis

Leaf of `.maestro/specs/releases/0.11.0.md`, section "Wave 3 — integration and proof" and the exit
gate "Security KPIs". The release spec's row is binding; this file exists so Maestro can track the
leaf as its own task.

Wave 3, last (needs the final stack, the published attestations and the Scorecard run).
