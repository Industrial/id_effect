---
title: 'Wave 8 — Security, bench and DX KPIs for 0.12: r12-w8-kpis'
slug: r12-w8-kpis
mode: heavy
work_type: spec-slice
risk_class: low
version: 1
acceptance_criteria:
  - 'docs/security/kpis-0.12.json: open highs (0), fuzz targets (≥ 13 total, ≥ 5 new, open crashes 0), compile-fail security cases (0.11 count + 2), secure-stack delta (≤ 5 µs), the Security-table tests green; scripts/security-kpis.sh learns the 0.12 fields and keeps its self-test.'
  - 'T12 bench results copied from docs/benchmarks/v0.12.json: unary delta, batch pair ratio, soak RSS ratio and heartbeat completeness, h3/h2 p99 ratio.'
  - 'docs/benchmarks/dx-kpis-0.12.json: K1–K6 with HTTP/3 on in the service template (K1 ≤ 180 s, K2 ≤ 8 s, K4 ≥ 17/20, K5 ≥ 80 %, K6 within budget) plus the cold-build delta of the http3 kit feature and of node quic pulling id_effect_platform (release spec Risk 9); measured on an idle machine.'
  - 'Gate: `bash scripts/security-kpis.sh --self-test && bash scripts/security-kpis.sh 0.12.0`; `DX_KPIS_RELEASE=0.12 bash scripts/dx-kpis.sh`'
  - 'Owns: `docs/security/kpis-0.12.json`, `scripts/security-kpis.sh`, `docs/benchmarks/dx-kpis-0.12.json`'
---

# r12-w8-kpis

Leaf of `.maestro/specs/releases/0.12.0.md`, section "E12.5 — Consumers and proof". The release spec's
row is binding; this file exists so Maestro can track the leaf as its own task.

Wave 8 (last, with r12-w8-level-audit). Blocked by: r12-w7-relay, r12-w7-ledger, r12-e12-5-benches,
r12-e12-5-fuzz-security. Run when no other agent is building (K2/K6 and benches need an idle machine).
