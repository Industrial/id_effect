---
title: 'Proof — Security and DX KPIs for 0.14 and the bench compare against v0.13.0: r14-x-kpis'
slug: r14-x-kpis
mode: heavy
work_type: spec-slice
risk_class: low
version: 1
acceptance_criteria:
  - 'docs/security/kpis-0.14.json via scripts/security-kpis.sh with a `0.14)` case in `configure_release` (scripts/security-kpis.sh:278-291 holds 0.13): open highs 0; fuzz targets ≥ 28 with ≥ 5 not in docs/security/kpis-0.13.json and no local crash input; compile-fail cases ≥ 23 (0.13 baseline 21 + 2) with `REQUIRED_CASES=(r4_upload_route_without_size_cap r7_profiling_endpoint_without_guard)`; secure-stack delta ≤ 5 µs and unary RPC delta ≤ 3 µs from docs/benchmarks/v0.14.json; every Security-table test of .maestro/specs/releases/0.14.0.md green (surface_014.rs); `security_releases` (sec.supply#7): every row of SECURITY.md "Security releases" has a GHSA id, verified against the published advisory, or the KPI stays `pending` with the owner step named.'
  - 'docs/benchmarks/dx-kpis-0.14.json via `DX_KPIS_RELEASE=0.14 bash scripts/dx-kpis.sh`: K1 ≤ 180 s, K2 ≤ 8 s, K4 ≥ 17/20, K5 ≥ 80 %, K6 within the 0.10 budget (120 s, docs/benchmarks/README.md), K3 recorded; plus `k6_kit_feature_deltas` for every kit feature new in 0.14 (at least the Kafka and AMQP features, measured as in dx-kpis-0.13.json: the `service` template copied once per feature, cold build delta) and the reference-app compile figures of docs/benchmarks/compile-reference-0.14.json (r14-p-toolchain) linked, not re-measured.'
  - 'Bench compare: `bash scripts/bench-compare.sh docs/benchmarks/v0.13.json docs/benchmarks/v0.14.json` passes (no budget/improve/delta/pair FAIL, no missing id) on the stored export of r14-e-benches; the output is attached to the leaf evidence and summarised for the CHANGELOG Performance section.'
  - 'Workspace coverage gate `bash scripts/llvm-cov-nextest.sh` ≥ 96 % regions with id_effect_kafka and id_effect_amqp included (container tests on); any new entry in `IGNORE_REGEX` (scripts/llvm-cov-nextest.sh:29) has a justification line.'
  - 'Tier: docs and scripts. Secure by default: the KPI file is the release''s security evidence; `pass: false` items are listed with their owner (as kpis-0.13.json: `openssf_scorecard`, `adr_0013_status`, `best_practices`, `slsa`).'
  - 'Coverage: not applicable to this leaf''s own files (scripts and JSON); the workspace gate above is the release''s coverage KPI.'
  - 'Gate: `bash scripts/security-kpis.sh --self-test && bash scripts/security-kpis.sh 0.14.0`; `DX_KPIS_RELEASE=0.14 DX_KPIS_DIR=/data/tmp/dx-kpis TMPDIR=/data/tmp bash scripts/dx-kpis.sh`; `bash scripts/dx-kpis.sh --self-test`; `bash scripts/bench-compare.sh docs/benchmarks/v0.13.json docs/benchmarks/v0.14.json`; `bash scripts/llvm-cov-nextest.sh`'
  - 'Owns: `docs/security/kpis-0.14.json`, `scripts/security-kpis.sh` (the 0.14 case and fields), `docs/benchmarks/dx-kpis-0.14.json`, `scripts/dx-kpis.sh` (0.14 release default and fields only), `scripts/llvm-cov-nextest.sh` (`IGNORE_REGEX` justifications only, if any)'
---

# r14-x-kpis

Leaf of `.maestro/specs/releases/0.14.0.md`, section "Proof". The release spec's row is binding;
this file exists so Maestro can track the leaf as its own task.

Wave 6 (with r14-x-level-audit). Blocked by: r14-e-benches, r14-e-fuzz-security, r14-p-toolchain
and r14-w-warden-ops. Idle machine; `DX_KPIS_DIR` and `TMPDIR` under `.tmp/` or `/data` (0.12 F61).
Feeds r14-x-rescore and r14-t0-migration-guide.

Defaults the owner may overturn: `security_releases` is verified against what is published and
stays `pending` (not `pass`) when the GHSA publication owner step has not happened, so the KPI
never claims sec.supply#7 on an unpublished advisory; kit-feature cold-build deltas are measured
for the features the 0.14 leaves add to id_effect_kit, whatever their final names.
