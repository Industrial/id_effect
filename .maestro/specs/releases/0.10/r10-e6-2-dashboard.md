---
title: 'E6.2 — Console, `id_effect_console`: r10-e6-2-dashboard'
slug: r10-e6-2-dashboard
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'Static page (`include_str!`, no JS toolchain, L12) polling the JSON API: fibers, supervisor tree, metrics, health, log-level form (#3). Integration test over a socket fetches `/_console` and every API route with and without the guard. Chapter + example.'
  - 'Gate: same + `cargo test --doc -p book_snippets`; `bash scripts/tier-conformance.sh id_effect_console`'
  - 'Owns: `crates/id_effect_console/src/ui/{index.html,mod.rs}`, `book/src/part6/ch27-03-console.md`, `crates/id_effect_console/examples/010_console.rs`, `crates/id_effect_console/tests/console.rs`'
---

# r10-e6-2-dashboard

Leaf of `.maestro/specs/releases/0.10.0.md`, section "E6.2 — Console, `id_effect_console` (L, wave 1 registry → wave 2) · obs.console #1 #2 #3 #6". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.
