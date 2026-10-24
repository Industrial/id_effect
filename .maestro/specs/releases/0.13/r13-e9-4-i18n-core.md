---
title: 'E9.4 — id_effect_i18n core: Fluent catalogs, CLDR plurals, per-request locale: r13-e9-4-i18n-core'
slug: r13-e9-4-i18n-core
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'D5: crates/id_effect_i18n over Project Fluent (`fluent-bundle`, `fluent-langneg`, `unic-langid`; exact pins recorded); every 0.13 feature and optional dependency declared once in its Cargo.toml (`axum`, `maud`, `schema`), so r13-e9-4-i18n-web does not edit the manifest.'
  - 'eco.i18n #1: `Catalog` of Fluent resources per locale, loaded from an allowlisted directory or embedded with `include_str!`; fallback chain (requested → language → default); a startup check reports every key missing from a non-default locale.'
  - 'eco.i18n #2: CLDR plural categories through Fluent selectors; tests cover en, de, pl, ar, ja.'
  - 'eco.i18n #7: `CurrentLocale` capability in the environment and `with_locale(locale, effect)` switching per request or fiber.'
  - 'Secure by default (R4): resources are parsed once at startup from trusted paths only (no locale string from input ever reaches a file path); parse errors fail startup.'
  - 'Tests `plural_categories_en_de_pl_ar_ja`, `missing_key_falls_back_to_default_locale`, `catalog_check_reports_missing_keys`, `with_locale_switches_per_fiber`, `catalog_rejects_path_outside_allowlist`; book `part6/ch30-09-i18n.md`.'
  - 'Tier: integration (new crate, ADR 0010 Amendment 9; moves to platform in r13-w6-pipeline). Coverage: `cargo llvm-cov nextest -p id_effect_i18n --all-features --summary-only --fail-under-regions 96`.'
  - 'Gate: `cargo nextest run -p id_effect_i18n`; `bash scripts/tier-conformance.sh id_effect_i18n`; `cargo deny check`'
  - 'Owns: `crates/id_effect_i18n/**` except `src/{negotiate,axum,schema_messages,maud}.rs` and their tests, `crates/id_effect/book/src/part6/ch30-09-i18n.md`'
---

# r13-e9-4-i18n-core

Leaf of `.maestro/specs/releases/0.13.0.md`, section "E9.4 — i18n". The release spec's row is binding;
this file exists so Maestro can track the leaf as its own task.

Wave 1. Blocked by: r13-t0-skeletons. Blocks r13-e9-4-i18n-web and r13-e-benches.
