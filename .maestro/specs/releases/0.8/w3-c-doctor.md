---
title: "0.8 W3-C — id-effect doctor and a layer graph (E1.4)"
slug: r08-w3-c-doctor
mode: light
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - '`id-effect doctor [--path DIR] [--json]` in crates/id_effect_tool/src/doctor/** reports pass/warn/fail per check and exits 1 on any fail: rustc >= the project rust-version (and the id_effect MSRV); every id_effect* crate in Cargo.lock / cargo metadata resolves to one version (no duplicate id_effect); keys listed in .env.example are set; DATABASE_URL (when present) is reachable over TCP; docker/compose present when the project has a Dockerfile/compose.yaml; the service port is free. Unit tests per check, and a fixture project with mixed id_effect versions that doctor fails.'
  - 'Layer graph (dx.diagnostics #7): crates/id_effect_kit/src/graph.rs lets an app register named layers (name + Layer, using the public TypeIds trait for In/Out capability lists) and renders the dependency edges (consumer In matched to producer Out) as text and DOT, flagging capabilities nobody provides; the service template exposes it with `--describe-layers` and `id-effect doctor --graph` runs that and prints it. Tests: a graph with a missing provider and a DOT golden.'
  - 'Book: a doctor/graph section (report the SUMMARY line); clippy -D warnings, rustdoc -D warnings, tests for id_effect_tool and id_effect_kit; tier-conformance PASS for both; template-smoke still green for the service template.'
non_goals:
  - Recording composition inside core Layer (needs merge/provide_to changes).
---

# 0.8 W3-C — doctor and layer graph

Row dx.diagnostics #5, #7 (0.6 had them via id-effect-diagnose, removed in 0.7). Release spec `.maestro/specs/releases/0.8.0.md`.
