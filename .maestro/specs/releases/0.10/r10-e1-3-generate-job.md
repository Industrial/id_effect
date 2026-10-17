---
title: 'E1.3r — `generate job`: r10-e1-3-generate-job'
slug: r10-e1-3-generate-job
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - '`id-effect generate job <name> [--queue Q] [--cron EXPR]` writes the `Job` impl, its registration and a test into a service project; template smoke generates one into a fresh `service` and runs `cargo test`.'
  - 'Gate: `cargo nextest run -p id_effect_tool`; `bash scripts/template-smoke.sh`'
  - 'Owns: `crates/id_effect_tool/src/generate/**`, `crates/id_effect_tool/templates/generate/job/**`, `src/cli.rs` (`Generate` variant, rebased after E1.5), `scripts/template-smoke.sh`, `book/src/part6/ch34-03-generators.md`'
---

# r10-e1-3-generate-job

Leaf of `.maestro/specs/releases/0.10.0.md`, section "E1.3r — `generate job` (S, wave 2, after `r10-e7-1-queue-core`) · dx.scaffold #4 (carry-over)". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.
