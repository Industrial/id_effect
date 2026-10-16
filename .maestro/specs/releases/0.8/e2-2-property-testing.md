---
title: 0.8 E2.2 — first-party property testing and restored test helpers
slug: r08-e2-2-property-testing
mode: light
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'test.property absent 4/8 -> documented 7/8: a feature-gated id_effect::testing property module (feature proptest, on the proptest crate) with generators derived from Schema/HasSchema (#2), effect-aware properties that run framework effects under TestExecutor (#4), stateful/model-based testing (#5), shipped generators for framework domain types (Duration, Chunk, Exit/Cause, HList caps where sensible) (#8); keeps shrinking/replay from proptest; book chapter, runnable example, integration test.'
  - 'test.unit 5/8 -> 7/8: first-party assertions for Exit/Cause (#3; the removed 0.6 assert_exit_* in a 0.8 form) and snapshot/golden testing support (#5; e.g. on insta behind a feature, or a small first-party golden helper).'
  - 'MIGRATION-0.8/CHANGELOG text reported (feature returns); clippy -D warnings, docs, tests green for id_effect with and without the feature.'
non_goals:
  - A separate id_effect_test crate (can be split out later).
---

# 0.8 E2.2 — property testing and test helpers

Rows test.property and test.unit (restores what 0.7 removed, MIGRATION-0.7.md:108). Release spec `.maestro/specs/releases/0.8.0.md`.
