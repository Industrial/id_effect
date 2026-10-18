---
title: 'E11.2 — Bounded and masked CSRF, rotation API, isolation headers: r11-sec-csrf-headers'
slug: r11-sec-csrf-headers
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'M4 (layer part): the CSRF layer reads a form body only for form content types, up to a configurable CsrfConfig::form_limit (default 64 KiB instead of FORM_LIMIT 2 MiB at crates/id_effect_axum/src/server/security/csrf.rs:40) and under a read timeout (default 10 s) at :225, independent of the order of other layers.'
  - 'L1 (BREACH): the token handed to pages and headers is masked per response (one-time pad XOR, base64url) and unmasked before the constant-time compare; MIGRATION-0.11 section "masked CSRF token format".'
  - 'L5 (API): CsrfToken::rotate issues a new token and invalidates the old one; r11-sec-auth-state calls it at login.'
  - 'L2 (headers): SecurityHeaders::recommended() (headers.rs:41-49) adds Cross-Origin-Opener-Policy: same-origin, Cross-Origin-Resource-Policy: same-origin and a deny-list Permissions-Policy (camera, microphone, geolocation, payment, usb), each overridable.'
  - 'Regression tests `m4_csrf_form_read_respects_limit`, `m4_csrf_form_read_times_out`, `l1_csrf_token_masked_differently_per_response`, `l1_masked_token_validates`, `l5_rotate_issues_new_token_and_invalidates_old`, `l2_recommended_headers_include_coop_corp_permissions_policy`.'
  - 'Gate: `cargo nextest run -p id_effect_axum --all-features`'
  - 'Owns: `crates/id_effect_axum/src/server/security/**`, `crates/id_effect_axum/tests/security_defaults.rs`'
---

# r11-sec-csrf-headers

Leaf of `.maestro/specs/releases/0.11.0.md`, section "E11.2 — Secure by default" (M4, L1, L2, L5).
The release spec's row is binding; this file exists so Maestro can track the leaf as its own task.

Wave 1. Blocked by: 0.10.1 r101-http (owned server/**). The template layer order (M4) is fixed by
r11-sec-ops-surfaces in wave 2.
