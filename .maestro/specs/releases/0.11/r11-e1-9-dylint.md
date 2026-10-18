---
title: 'E1.9 — Lints as a published dylint library (carry-over): r11-e1-9-dylint'
slug: r11-e1-9-dylint
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'labs/id_effect_lint (publish = false at Cargo.toml:2, nightly rustc_private at :22) moves to lints/id_effect_lints, outside the main workspace with its own rust-toolchain.toml (pinned nightly); the rustc lint plugins are ported to dylint_linting; the ast-grep rules stay usable (sgconfig.yml).'
  - 'dx.lint#3: installable as a package — published to crates.io as id_effect_lints (r11-sc-publish adds the publish step; crate name availability recorded) and loadable through `[workspace.metadata.dylint] libraries = [{ git = ..., tag = ... }]`; #4: runs through `cargo dylint` (the clippy-style driver); #5: at least three lints emit MachineApplicable suggestions that `cargo dylint --fix` applies.'
  - 'New lint raw_sql_non_literal warns when a SqlClient query/execute (&str) receives a non-literal (0.11.0.md D9, ADR 0013 R5 second enforcement point).'
  - 'dylint_testing UI tests per lint; .github/workflows/lint.yml runs `cargo dylint --all` on the workspace (SHA-pinned, permissions read); book appendix-d-workspace-tooling.md documents install and use; level labs → documented (5/8 + #5 = 6/8 planned, target 63).'
  - 'Gate: `cd lints/id_effect_lints && cargo test`; `cargo dylint --path lints/id_effect_lints --all -- --workspace`; `bash scripts/check-workflows.sh`'
  - 'Owns: `lints/id_effect_lints/**`, `labs/id_effect_lint/**` (deleted), `.github/workflows/lint.yml`, `crates/id_effect/book/src/appendix-d-workspace-tooling.md`'
---

# r11-e1-9-dylint

Leaf of `.maestro/specs/releases/0.11.0.md`, section "E11.4 part 1, E1.9, E2.6". The release spec's
row is binding; this file exists so Maestro can track the leaf as its own task.

Wave 1. Carry-over from 0.10 (D1 checkpoint moved dx.lint 63 to 0.11.0). Risk 10 in the release
spec: if the assessor does not accept the library as published, dx.lint misses (−0.3 E) and needs
a written carry-over. r11-w3-kit-template adds one step (check-dangerous.sh) to lint.yml in wave 3.
