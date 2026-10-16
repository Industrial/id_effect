---
title: 0.8 E2.4 + E1.8 — seeded interleaving exploration; idiom guide
slug: r08-e2-4-determinism-idioms
mode: light
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'test.determinism 6/8 -> 7/8: seeded exploration of task interleavings in TestExecutor (#7): a seed selects the order in which ready fibers run, a helper runs a test under N seeds and reports the failing seed, and re-running with that seed reproduces it; a test that finds a planted race; book section.'
  - 'core.ergonomics 5/8 -> 6/8: official idiom/style guide for application code (#8) in the book (when to use effect!, Layer vs provide, error modelling, retry factories, resource scopes, testing), every snippet compiled (mdbook test or doctest harness used by the book).'
  - 'clippy -D warnings, docs, tests green for id_effect.'
non_goals:
  - Randomised test order across the test harness (#8).
---

# 0.8 E2.4 + E1.8

Rows test.determinism and core.ergonomics. Release spec `.maestro/specs/releases/0.8.0.md`.
