---
title: 'E5.2b — OIDC + PKCE, TOTP: r09-e5-2b-totp'
slug: r09-e5-2b-totp
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'Enrol (secret + otpauth URI), verify with ±1 step skew under the executor clock (TestExecutor-testable), recovery codes hashed with argon2.'
  - 'Gate: `cargo nextest run -p id_effect_auth --all-features`'
  - 'Owns: `crates/id_effect_auth/src/totp.rs`'
---

# r09-e5-2b-totp

Leaf of `.maestro/specs/releases/0.9.0.md`, section "E5.2b — OIDC + PKCE, TOTP (M, wave 2, after E5.2a) · sec.authn #5 #6". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.
