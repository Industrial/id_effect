---
title: 'E2.6 — Bench gates in CI and the secure-stack ratio: r11-e2-6-bench-ci'
slug: r11-e2-6-bench-ci
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'test.qa#8: .github/workflows/bench.yml runs scripts/bench-run.sh for base and head on the same runner (dedicated `bench` runner label if available, otherwise ubuntu-latest) on pushes to main and on pull requests labelled perf, then scripts/bench-compare.sh against docs/benchmarks/gates.toml; a failed budget gate fails the job; SHA-pinned, permissions read.'
  - 'crates/id_effect_axum/benches/gate_request.rs gains a secure_stack case (SecurityLayer::recommended, rate limit, load shedding, authentication + guard, default log redaction) next to the bare request; gates.toml gains a `kind = "pair"` gate secure_stack / bare ≤ 1.15 (an intra-run ratio, which bench-compare.sh learns, with --self-test cases).'
  - 'docs/benchmarks/README.md documents the job and the pair gate. The secure-stack figure is re-measured with the final stack by r11-w3-kpis.'
  - 'Gate: `bash scripts/bench-compare.sh --self-test`; `bash scripts/bench-run.sh --only id_effect_axum:gate_request`; `bash scripts/check-workflows.sh`'
  - 'Owns: `.github/workflows/bench.yml`, `crates/id_effect_axum/benches/gate_request.rs`, `docs/benchmarks/{gates.toml,README.md}`, `scripts/bench-compare.sh`'
---

# r11-e2-6-bench-ci

Leaf of `.maestro/specs/releases/0.11.0.md`, section "E11.4 part 1, E1.9, E2.6". The release spec's
row is binding; this file exists so Maestro can track the leaf as its own task.

Wave 1. 0.10 stretch item taken into 0.11 by D1 because the T11 KPI "secure-stack overhead ≤ 1.15×"
needs a CI bench. The secure_stack case grows as the wave-1/2 security layers land.
