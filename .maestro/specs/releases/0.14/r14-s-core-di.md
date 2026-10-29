---
title: 'E13.1 — Sweep — Layer graph cycle detection and build order: r14-s-core-di'
slug: r14-s-core-di
mode: heavy
work_type: spec-slice
risk_class: low
version: 1
acceptance_criteria:
  - 'core.di#8 (row core.di 88 → 100). What exists: the renderable dependency graph is `id_effect_kit::graph::LayerGraph` (crates/id_effect_kit/src/graph.rs:140; `edges` :187, `missing` :204, `to_text` :229, `to_dot` :259, `describe_if_requested` :312) and `id-effect doctor --graph` reads it (crates/id_effect_tool/src/doctor/graph.rs:20 `check`, :79 `summary`), E1.4. What is missing: cycle detection. A `Layer` records no graph (graph.rs:4-5, crates/id_effect/src/layer.rs:187), and typed composition (`merge` / `provide_to` / `provide_merge`, layer.rs:381-431) cannot build a cyclic layer, so a cycle shows up only as two registered layers that each need what the other provides, which the compiler reports as an unsatisfiable `provide_*` far from the cause. Built: `LayerGraph::cycles() -> Vec<Cycle<''_>>` (each strongly connected component of more than one layer over the provider-to-consumer edges, reported once, its layers in registration order with the capabilities on its edges) and `LayerGraph::build_order() -> Result<Vec<&str>, Vec<Cycle<''_>>>` (providers before consumers, registration order among equals).'
  - 'Rendering: `to_text` and `to_dot` keep their output byte for byte for an acyclic graph (the summary line `layer graph: N layers, N edges, N missing` is pinned by crates/id_effect_kit/tests/golden/layers.dot, book part6/ch34-01-doctor-graph.md, the tutorial and examples/http-showcase); a cyclic graph appends `, N cycles` to the summary, a `cycle: a -> b -> a` line per cycle in text, and draws cycle edges `color=orange` in DOT. `id-effect doctor --graph` fails the graph check when the summary names cycles (doctor/graph.rs:67 decides on missing only today) and still reads a summary without the cycles part (binaries built against 0.13 kit).'
  - 'Tests: unit tests in crates/id_effect_kit/src/graph.rs `cycles_reports_each_cycle_once_in_registration_order`, `an_acyclic_graph_has_no_cycles_and_a_build_order`, `build_order_puts_providers_first`, `a_self_satisfied_layer_is_not_a_cycle` (graph.rs:489 `a_layer_does_not_provide_its_own_need` semantics kept); crates/id_effect_kit/tests/graph.rs `cyclic_graph_text_and_dot_name_the_cycle` (new golden crates/id_effect_kit/tests/golden/layers_cycle.dot) and `acyclic_rendering_is_unchanged` (existing golden layers.dot unchanged); crates/id_effect_tool/src/doctor/graph.rs `graph_check_fails_on_a_cycle`, `graph_check_reads_a_summary_without_cycles`.'
  - 'Book: crates/id_effect/book/src/part6/ch34-01-doctor-graph.md section "Cycles and build order"; crates/id_effect/book/src/part2/ch06-04-layer-graphs.md one paragraph linking it. Example: the module doctest of graph.rs gains a cycle case (no new example file; the kit has one example, 010_prelude.rs).'
  - 'Tier: platform (id_effect_kit, ADR 0010 Amendment 4) and integration (id_effect_tool); no core change. Secure by default: no new surface (`--describe-layers` already exists and prints type names only).'
  - 'Coverage: `cargo llvm-cov nextest -p id_effect_kit --all-features --summary-only --fail-under-regions 96`; `cargo llvm-cov nextest -p id_effect_tool --all-features --summary-only --fail-under-regions 96`'
  - 'Gate: `cargo nextest run -p id_effect_kit -p id_effect_tool --all-features`; `cargo test --doc -p id_effect_kit`; `cargo clippy -p id_effect_kit -p id_effect_tool --all-targets --all-features -- -D warnings`'
  - 'Owns: crates/id_effect_kit/src/graph.rs, crates/id_effect_kit/tests/graph.rs, crates/id_effect_kit/tests/golden/layers_cycle.dot (new), crates/id_effect_tool/src/doctor/graph.rs, crates/id_effect/book/src/part6/ch34-01-doctor-graph.md (new section only), crates/id_effect/book/src/part2/ch06-04-layer-graphs.md (one paragraph), CHANGELOG.md (append-only)'
---

# r14-s-core-di

Leaf of `.maestro/specs/releases/0.14.0.md`, section "E13.1 — Sweep". The release spec's row is binding;
this file exists so Maestro can track the leaf as its own task.

Wave 1. Blocked by: r14-t0-skeletons (it edits crates/id_effect_kit/Cargo.toml for the `kafka` / `amqp`
features; this leaf does not touch the manifest). Blocks: r14-x-level-audit, r14-x-rescore. Shares no
file with r14-s-core-runtime (wave 1): this leaf edits nothing under crates/id_effect/src.

Defaults the owner may overturn: (1) cycle detection lives where the graph already lives, in
id_effect_kit; `LayerGraph` is not moved into core (core stays slim per ADR 0010, and moving it would
be a breaking path change for every app calling `describe_if_requested`). If the scorer insists that
core.di evidence be in crates/id_effect, the alternative is a thin `crates/id_effect/src/layer_graph.rs`
with `cycles` over `TypeIds` (layer.rs:578) re-used by the kit; that would put this leaf into
crates/id_effect/src in wave 1 next to r14-s-core-runtime, owning only that new file. (2) The summary
line changes only when a cycle exists, so no golden, book page or example needs re-blessing.
Non-goals: recording a graph automatically from `merge` / `provide_to` (a `Layer` keeps only its build
function), runtime cycle detection during `build`.
