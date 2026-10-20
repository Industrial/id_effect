---
title: 'T0 — ADR 0010 Amendment 7 and crate skeletons: r11-t0-skeletons'
slug: r11-t0-skeletons
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'ADR 0010 Amendment 7 (after Amendment 6, docs/adrs/0010-crate-tiers.md:182) names: id_effect_workflow (published, tier integration; replaces labs/id_effect_workflow, which r11-e7-5-sagas-promote deletes); STM moving from id_effect_fp (maintenance) into id_effect core with a one-minor deprecated re-export in id_effect_fp (L9, 0.11.0.md D5); the unpublished test crate tests/security_compile_fail (publish = false).'
  - 'Root Cargo.toml members gain crates/id_effect_workflow and tests/security_compile_fail; scripts/validate-crate-packaging.sh LEVEL/ORDER: `--print-order` lists id_effect_workflow after id_effect_sql and id_effect_jobs and before id_effect_kit; the security test crate is never published.'
  - 'crates/id_effect_workflow skeleton: Cargo.toml on the workspace version, README with a `rust` fence, `#![deny(missing_docs)]`, one example, one test, `[package.metadata.id_effect] tier = "integration"`; crates.io availability of `id_effect_workflow` recorded in the leaf evidence (generic User-Agent, no personal data).'
  - 'docs/MIGRATION-0.11.md created with one heading per planned break (H8 routing, Sql/RawSql, process Next/Info/SendError, RedactionPolicy::new, Secret<T: Zeroize> and its equality, STM path, SSRF default, plaintext TCP feature, CSRF masked token format, mesh protocol version); each owning leaf fills its section.'
  - 'Gate: `bash scripts/tier-conformance.sh id_effect_workflow`; `bash scripts/validate-crate-packaging.sh`; `cargo nextest run -p id_effect_workflow -p security_compile_fail`'
  - 'Owns: `docs/adrs/0010-crate-tiers.md`, root `Cargo.toml` (members), `crates/id_effect_workflow/**` (skeleton only; r11-e7-5-engine owns it next), `tests/security_compile_fail/**` (skeleton only; r11-e11-4-compile-fail owns it in wave 3), `scripts/validate-crate-packaging.sh`, `docs/MIGRATION-0.11.md` (headings)'
---

# r11-t0-skeletons

Leaf of `.maestro/specs/releases/0.11.0.md`, section "§7.0b and T0". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.

Wave 1 head (day 0, alongside r11-m22-rebase and r11-sec-adr0013). Blocked by: the 0.10.1 tag merged
into the release branch. r11-e7-5-engine and r11-e11-4-compile-fail build on the skeletons.
