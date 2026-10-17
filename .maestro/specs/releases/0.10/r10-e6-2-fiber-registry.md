---
title: 'E6.2 — Console, `id_effect_console`: r10-e6-2-fiber-registry'
slug: r10-e6-2-fiber-registry
mode: heavy
work_type: spec-slice
risk_class: high
version: 1
acceptance_criteria:
  - 'D8; `fiber_dump()` returns id, parent id, state, current span name, age for live fibers; entries vanish when the fiber ends. Tests `dump_lists_running_and_drops_finished`, `parent_links_follow_fork`. Bench gates and `kernel_alloc` green with the feature off and on.'
  - 'Gate: `cargo nextest run -p id_effect --features fiber-registry`; `cargo nextest run -p id_effect --test kernel_alloc`; `scripts/bench-compare.sh` (no budget fail)'
  - 'Owns: `crates/id_effect/src/runtime/{fiber_context.rs,fiber_registry.rs}`, `crates/id_effect/Cargo.toml` (feature `fiber-registry`)'
---

# r10-e6-2-fiber-registry

Leaf of `.maestro/specs/releases/0.10.0.md`, section "E6.2 — Console, `id_effect_console` (L, wave 1 registry → wave 2) · obs.console #1 #2 #3 #6". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.
