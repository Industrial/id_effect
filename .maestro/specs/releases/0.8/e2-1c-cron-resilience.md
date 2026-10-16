---
title: 0.8 E2.1c+r — cron schedules; resilience fallback and policy pipeline
slug: r08-e2-1c-cron-resilience
mode: light
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'msg.cron 2/8 -> 6/8: cron-expression schedules (#1) with timezone- and DST-aware next-fire computation (#3, via the existing jiff-based scheduling/datetime.rs), overlap/missed-run policy skip|catch-up (#5), run monitoring hooks: success/failure callbacks and run history (#7), declarative central registration of named cron jobs (#8); built on Schedule/fibers (effect-native), tested with TestExecutor virtual time including a DST transition and a missed-run case.'
  - 'fault.resilience 6/8 level tested -> 7/8 level documented: fallback policy (#7) in id_effect_resilience, plus #8 if cheap (policies compose into one reusable pipeline with telemetry); book chapter section and a runnable example; integration test.'
  - 'Book: cron chapter or section and resilience section; examples runnable; clippy -D warnings, docs, tests green for touched crates.'
non_goals:
  - Persistent / runtime-mutable schedules (#6) — needs the 0.10 jobs store.
---

# 0.8 E2.1c+r — cron and resilience

Rows msg.cron and fault.resilience. Release spec `.maestro/specs/releases/0.8.0.md`.
