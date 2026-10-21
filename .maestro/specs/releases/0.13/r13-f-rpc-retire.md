---
title: 'T12 carry (F46) — `id-effect rpc check --allow-retired Service:N`: r13-f-rpc-retire'
slug: r13-f-rpc-retire
mode: heavy
work_type: spec-slice
risk_class: low
version: 1
acceptance_criteria:
  - 'F46: `id-effect rpc check --contract <file> --against <ref|file> --allow-retired Service:N` (repeatable) lets the named removed service version pass, printing it as "retired"; every other breaking change still exits 1; an `--allow-retired` naming a version that was not removed is an error (typo guard).'
  - 'The argument is parsed in crates/id_effect_tool/src/rpc/mod.rs (the rpc subcommand args) and applied in src/rpc/check.rs; cli.rs is not edited.'
  - 'Tests `retired_version_allowed_with_flag`, `retired_flag_does_not_hide_other_breaks`, `retired_flag_for_unknown_version_is_error`; book ch33-12 gains one paragraph.'
  - 'Tier: integration (id_effect_tool). Secure by default: no new surface (a stricter check, not a weaker one).'
  - 'Coverage: `cargo llvm-cov nextest -p id_effect_tool --all-features --summary-only --fail-under-regions 96`.'
  - 'Gate: `cargo nextest run -p id_effect_tool --test rpc_check`'
  - 'Owns: `crates/id_effect_tool/src/rpc/{mod.rs,check.rs}`, `crates/id_effect_tool/tests/rpc_check.rs` (new cases), one paragraph of `crates/id_effect/book/src/part6/ch33-12-contract-check.md`'
---

# r13-f-rpc-retire

Leaf of `.maestro/specs/releases/0.13.0.md`, section "T12 carry" (0.12 overlay F46). The release spec's
row is binding; this file exists so Maestro can track the leaf as its own task.

Wave 4. Blocked by: the v0.12.0 tag. Runs beside r13-e1-3-generate-admin (tool `generate.rs`, a
different file). In the scope-cut order (moves no row).
