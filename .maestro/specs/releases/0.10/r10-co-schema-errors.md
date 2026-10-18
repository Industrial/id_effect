---
title: 'CO — 0.9 carry-over fixes: r10-co-schema-errors'
slug: r10-co-schema-errors
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'Derived schemas accumulate all field errors like combinator schemas (`decode_unknown_all`); trybuild/ui snapshots updated. Test `derived_schema_reports_every_invalid_field`.'
  - 'Gate: `cargo nextest run -p id_effect_proc_macro -p id_effect`'
  - 'Owns: `crates/id_effect_proc_macro/src/derive_schema_parser.rs`'
---

# r10-co-schema-errors

Leaf of `.maestro/specs/releases/0.10.0.md`, section "CO — 0.9 carry-over fixes (S, wave 1)". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.
