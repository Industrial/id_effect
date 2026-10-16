---
title: 'E5.2b — OIDC + PKCE, TOTP: r09-e5-2b-oidc'
slug: r09-e5-2b-oidc
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'Discovery, authorization-code + PKCE, state/nonce in the session, ID-token validation, callback handler; `OAuthClient` implemented for it. Integration test logs in against a Keycloak container (D5).'
  - 'Gate: `ID_EFFECT_REQUIRE_DOCKER=1 cargo nextest run -p id_effect_auth --features oidc`'
  - 'Owns: `crates/id_effect_auth/src/oidc.rs`'
---

# r09-e5-2b-oidc

Leaf of `.maestro/specs/releases/0.9.0.md`, section "E5.2b — OIDC + PKCE, TOTP (M, wave 2, after E5.2a) · sec.authn #5 #6". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.
