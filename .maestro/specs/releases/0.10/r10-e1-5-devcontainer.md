---
title: 'E1.5 — `id-effect dev`: r10-e1-5-devcontainer'
slug: r10-e1-5-devcontainer
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'Generated projects ship `.devcontainer/devcontainer.json` (+ compose services) (#8); template smoke builds it with `devcontainer read-configuration`-free JSON validation in the test; chapter.'
  - 'Gate: `cargo nextest run -p id_effect_tool`; `bash scripts/template-smoke.sh`'
  - 'Owns: `crates/id_effect_tool/templates/common/_devcontainer/**`, `src/template.rs` (`FILES`), `crates/id_effect_tool/tests/bin.rs`, `book/src/part6/ch34-02-dev.md`'
---

# r10-e1-5-devcontainer

Leaf of `.maestro/specs/releases/0.10.0.md`, section "E1.5 — `id-effect dev` (M, wave 2) · dx.repl #2 #3 #8, fault.reload #1". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.
