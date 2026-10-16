---
title: 0.8 E2.1s — stream operators zip/combine_latest, throttle/debounce, group_by
slug: r08-e2-1s-stream-ops
mode: light
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'msg.streams 5/8 -> at least 7/8: zip / zip_with / combine_latest (#4), time-based throttle and debounce (#5) on the fiber clock (TestExecutor virtual-time tests), group_by key into substreams/partitions (#6) with bounded buffering and backpressure; all interruption-safe (finalizers run when the consumer stops early).'
  - 'Book streams chapter updated with each operator; a runnable example; clippy -D warnings, docs, tests green for id_effect.'
non_goals:
  - Changing existing operator semantics.
---

# 0.8 E2.1s — stream operators

Row msg.streams. Release spec `.maestro/specs/releases/0.8.0.md`.
