---
title: 'HTTP edge (H1, H2, M1): r101-http'
slug: r101-http
mode: heavy
work_type: spec-slice
risk_class: high
version: 1
acceptance_criteria:
  - 'Closes H1, H2 and M1 as specified in the Findings table of .maestro/specs/releases/0.10.1.md, within the patch-release rules there (additive API only; stricter defaults with a named dangerous/insecure opt-out). One regression test per finding, named after it (for example h1_per_ip_limit_sees_the_peer).'
  - 'Gate: cargo test -p id_effect_axum --all-features; cargo test -p ledger (examples/reference/ledger, needs Postgres); cargo clippy -p id_effect_axum --all-features --all-targets -- -D warnings'
  - 'Owns: crates/id_effect_axum/src/server/**, crates/id_effect_axum/src/middleware/rate_limit.rs, crates/id_effect_axum/src/client_ip.rs, crates/id_effect_axum/src/lib.rs (re-export only), crates/id_effect_axum/Cargo.toml (hyper-util features only), crates/id_effect_axum/tests/**, examples/reference/ledger/src/app.rs, examples/reference/ledger/config/**'
---

# r101-http

Leaf of `.maestro/specs/releases/0.10.1.md`. The release spec's Findings rows for H1, H2 and M1 are binding;
this file exists so Maestro can track the leaf as its own task.
