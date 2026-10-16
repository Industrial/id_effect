---
title: 0.8 E2.6 — measure and publish compile time of the template app
slug: r08-e2-6-compile-time
mode: light
work_type: feature
risk_class: low
version: 1
acceptance_criteria:
  - 'New scripts/compile-time.sh (bash, Requires header, exit 1 FAIL / 2 misuse, --self-test without building) times a cold build of examples/cli-minimal in a fresh CARGO_TARGET_DIR with no RUSTC_WRAPPER, an incremental rebuild after a one-line change, and a run of the binary; median over --runs N; --json emits {crate, cold_s, incremental_s, boot_s, runs, rustc, wrapper}.'
  - 'docs/benchmarks/compile-0.8.json committed with machine info; docs/benchmarks/README.md documents the method; moon task compile-time exists outside the default ci path.'
  - 'dx.compile 0.8 evidence: #7 (measured and published) and #2 (cold build < 60 s) only if the measured median shows it; #1/#6 credited only if their measured numbers meet the thresholds.'
non_goals:
  - Watch mode or hot reload (E1.5, 0.10).
---

# 0.8 E2.6 — compile-time measurement

Moves dx.compile 1/7 -> 3/7 if the numbers allow. Release spec: `.maestro/specs/releases/0.8.0.md`.
