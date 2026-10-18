---
title: 'Mesh transport and handshake (H3, H4, M9): r101-mesh'
slug: r101-mesh
mode: heavy
work_type: spec-slice
risk_class: high
version: 1
acceptance_criteria:
  - 'Closes H3, H4 and M9 as specified in the Findings table of .maestro/specs/releases/0.10.1.md, within the patch-release rules there (additive API only; stricter defaults with a named dangerous/insecure opt-out). One regression test per finding, named after it (for example h1_per_ip_limit_sees_the_peer).'
  - 'Gate: cargo test -p id_effect_node --all-features; cargo clippy -p id_effect_node --all-features --all-targets -- -D warnings'
  - 'Owns: crates/id_effect_node/src/transport/**, crates/id_effect_node/src/handshake.rs, crates/id_effect_node/src/session.rs, crates/id_effect_node/src/config*.rs, crates/id_effect_node/tests/**, crates/id_effect_node/Cargo.toml (rustls features only)'
---

# r101-mesh

Leaf of `.maestro/specs/releases/0.10.1.md`. The release spec's Findings rows for H3, H4 and M9 are binding;
this file exists so Maestro can track the leaf as its own task.
