---
title: "0.8 W3-E4 — book snippets with real dependencies instead of stand-ins; recover dependency-blocked fences"
slug: r08-w3-e4-snippet-deps
mode: light
work_type: spec-slice
risk_class: low
version: 1
acceptance_criteria:
  - 'examples/book-snippets/Cargo.toml gains the crates the remaining dependency-blocked fences need: tokio (macros, rt, rt-multi-thread, signal), clap (derive), proptest, reqwest (json, no default TLS if it compiles), tower (util) + id_effect_axum feature tower, id_effect_logger, opentelemetry + opentelemetry_sdk (versions matching id_effect_opentelemetry), cli_minimal (path).'
  - 'The hidden tokio stand-in in part1/ch01-01 and the hidden clap stand-in in cookbook/ch40-03 are replaced by the real crates; fences ignored only for "needs dep" reasons compile (no_run where they would block or need a network/collector); remaining ignores keep a reason (labs crates, type-definition copies, quick-reference list).'
  - 'cargo test --doc -p book_snippets green; moon run :book green; report the new compiled/total count.'
non_goals:
  - Labs crates (outside the workspace).
---

# 0.8 W3-E4 — snippet dependencies

KPI K5 follow-up to W3-E1..E3. Release spec `.maestro/specs/releases/0.8.0.md`.
