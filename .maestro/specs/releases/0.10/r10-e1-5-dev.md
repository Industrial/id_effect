---
title: 'E1.5 — `id-effect dev`: r10-e1-5-dev'
slug: r10-e1-5-dev
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - '`id-effect dev [--services] [--watch PATH…]`: polls mtimes (D4), `cargo build`, restarts the child with `ID_EFFECT_PROFILE=dev` after SIGTERM + drain (#2, #3, fault.reload#1); a failed build keeps the old child and prints the errors; `--services` runs `docker compose up -d` once. Tests `edit_triggers_rebuild_and_restart`, `build_error_keeps_previous_child`.'
  - 'Gate: `cargo nextest run -p id_effect_tool`'
  - 'Owns: `crates/id_effect_tool/src/dev.rs`, `src/cli.rs` (`Dev` variant), `crates/id_effect_tool/tests/dev.rs`'
---

# r10-e1-5-dev

Leaf of `.maestro/specs/releases/0.10.0.md`, section "E1.5 — `id-effect dev` (M, wave 2) · dx.repl #2 #3 #8, fault.reload #1". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.
