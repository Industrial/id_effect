---
title: 'E8.3 — Cluster-wide name registry: r11-e8-3-global-registry'
slug: r11-e8-3-global-registry
mode: heavy
work_type: spec-slice
risk_class: high
version: 1
acceptance_criteria:
  - 'dist.placement#5: GlobalRegistry with register(name, pid), whereis(name), unregister(name), resolvable from every node (today registry.rs:150 registers behaviours, not processes); an entry is removed when its process exits or its node goes down; registering a taken name fails with AlreadyRegistered.'
  - 'After a partition heals, conflicting registrations resolve deterministically to one owner (documented rule, e.g. lowest (incarnation, node id)); the losing process receives an exit signal with reason NameConflict; an Addr can be obtained by global name.'
  - 'Tests `global_name_resolves_from_every_node`, `name_released_on_process_exit`, `name_released_on_node_down`, `conflicting_registration_after_heal_keeps_one`, `register_twice_fails_with_already_registered`; example 135_global_names.rs; book part7/ch39-01-global-registry.md.'
  - 'Gate: `cargo nextest run -p id_effect_node --all-features --test global_registry`; `cargo nextest run -p id_effect_node --all-features`'
  - 'Owns: `crates/id_effect_node/src/{global_registry.rs,lib.rs,envelope.rs,mesh.rs}`, `crates/id_effect_node/tests/global_registry.rs`, `crates/id_effect_node/examples/135_global_names.rs`, `crates/id_effect/book/src/part7/ch39-01-global-registry.md`'
---

# r11-e8-3-global-registry

Leaf of `.maestro/specs/releases/0.11.0.md`, section "E8.3 — Mesh hardening". The release spec's row
is binding; this file exists so Maestro can track the leaf as its own task.

Wave 1. Blocked by: 0.10.1 r101-mesh. r11-e8-3-reliable-delivery takes over envelope.rs, mesh.rs and
lib.rs in wave 2.
