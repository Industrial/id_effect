---
title: 0.8 E2.3b — derived Schema exports its JSON Schema; real regex validator
slug: r08-e2-3b-schema-followups
mode: light
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - '#[derive(Schema)] (crates/id_effect_proc_macro/src/derive_schema_parser.rs, built through Schema::make) attaches a SchemaAst via .with_ast(..) built from each field''s HasSchema::schema().ast() (struct with field names, optional/nullable fields, enums/unions where the derive supports them), so json_schema_for::<Derived>() is no longer {}; golden tests for a derived struct, a nested derived struct and an enum if supported; existing derive tests unchanged.'
  - 'Real regular expressions for data.schema #8: a regex-backed validator (e.g. schema::validators::regex(pattern) -> Result<..>) using the regex crate as an optional or default dependency of id_effect (decide and justify; tier-conformance PASS), exporting the same pattern into JSON Schema; the existing caller-supplied-matcher pattern() stays.'
  - 'Book schema chapter updated; cargo test/clippy -D warnings/doc for id_effect and id_effect_proc_macro green with default and all features.'
non_goals:
  - Schema-driven generators in testing::property (may use the derived AST later).
---

# 0.8 E2.3b — schema follow-ups

Follow-ups from E2.3 (commit 18df824) and E2.2 (commit 2ce6900). Release spec `.maestro/specs/releases/0.8.0.md`.
