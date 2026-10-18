---
title: 'HTTP client body cap (M2): r101-client'
slug: r101-client
mode: heavy
work_type: spec-slice
risk_class: high
version: 1
acceptance_criteria:
  - 'Closes M2 as specified in the Findings table of .maestro/specs/releases/0.10.1.md, within the patch-release rules there (additive API only; stricter defaults with a named dangerous/insecure opt-out). One regression test per finding, named after it (for example h1_per_ip_limit_sees_the_peer).'
  - 'Gate: cargo test -p id_effect_platform --all-features; cargo clippy -p id_effect_platform --all-features --all-targets -- -D warnings'
  - 'Owns: crates/id_effect_platform/src/http/**, crates/id_effect_platform/tests/**'
---

# r101-client

Leaf of `.maestro/specs/releases/0.10.1.md`. The release spec's Findings rows for M2 are binding;
this file exists so Maestro can track the leaf as its own task.
