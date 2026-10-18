---
title: 'Login, TOTP, JWT and hashing limits (H5, H6, H7, M15, M20, L4): r101-auth'
slug: r101-auth
mode: heavy
work_type: spec-slice
risk_class: high
version: 1
acceptance_criteria:
  - 'Closes H5, H6, H7, M15, M20 and L4 as specified in the Findings table of .maestro/specs/releases/0.10.1.md, within the patch-release rules there (additive API only; stricter defaults with a named dangerous/insecure opt-out). One regression test per finding, named after it (for example h1_per_ip_limit_sees_the_peer).'
  - 'Gate: cargo test -p id_effect_auth --all-features (accounts tests need Postgres); cargo clippy -p id_effect_auth --all-features --all-targets -- -D warnings'
  - 'Owns: crates/id_effect_auth/src/basic.rs, totp.rs, jwt.rs, jwks.rs, password.rs, new throttle.rs, account/** (login wiring only), lib.rs (re-exports only), crates/id_effect_auth/tests/**'
---

# r101-auth

Leaf of `.maestro/specs/releases/0.10.1.md`. The release spec's Findings rows for H5, H6, H7, M15, M20 and L4 are binding;
this file exists so Maestro can track the leaf as its own task.
