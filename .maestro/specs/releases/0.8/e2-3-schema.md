---
title: 0.8 E2.3 — JSON Schema export; accumulating validation and built-in validators
slug: r08-e2-3-schema
mode: light
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'data.jsonschema 0/8 -> 4/8, level tested: JSON Schema (2020-12) export for any Schema/HasSchema type (#1; first establish what crates/id_effect/src/schema/json_schema_export.rs already does and why the 0.7 scorecard did not credit it), the same schema value used for validation and export (#4), descriptions/examples/deprecation annotations emitted (#5), constraints reflected (minLength, pattern, enum, ranges) (#6); golden tests per schema kind; validated against a JSON Schema validator crate in dev-dependencies if one is already in the tree, else golden only.'
  - 'data.schema 6/8 -> 7/8: struct/record decoding accumulates all field errors in one pass instead of fail-fast (#6, parse.rs ~:500), or built-in validators email/uuid/url/length/range/regex (#8) — do #6, add #8 if cheap; existing parse tests keep passing; MIGRATION-0.8 text if error shapes change.'
  - 'Book schema chapter updated; clippy -D warnings, docs, tests green for id_effect.'
non_goals:
  - OpenAPI from routes and docs UI (0.9, E4.1/E4.2).
---

# 0.8 E2.3 — schema

Rows data.jsonschema (0.8 target 50, owner decision D1) and data.schema. Release spec `.maestro/specs/releases/0.8.0.md`.
