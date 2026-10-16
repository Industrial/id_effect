---
title: 0.8 E5.1 — web hardening (CSRF, CORS, CSP nonces, security headers, secure cookies)
slug: r08-e5-1-web-hardening
mode: light
work_type: feature
risk_class: high
version: 1
acceptance_criteria:
  - 'crates/id_effect_axum/src/server/security.rs becomes server/security/{mod,csrf,cors,csp,headers,cookies}.rs with the 0.7 public names re-exported; 0.7 CsrfConfig::new / csrf_middleware / csp_middleware / set_csrf_header stay one minor as #[deprecated] shims, and the deprecated CSRF path also switches to a subtle constant-time compare (fixes the == at security.rs:86-90).'
  - 'CSRF (#1): 32-byte CSPRNG token per client session in a signed __Host- cookie (SameSite=Strict, Secure, Path=/), echoed in X-CSRF-Token or a form field, compared with subtle::ConstantTimeEq; CsrfToken extractor; tests csrf_tokens_are_random_per_session, csrf_rejects_mismatched_and_forged_token.'
  - 'Default-on (#2): SecurityLayer::recommended(key) bundles CSRF (on), security headers, CSP with nonce and cookie defaults, with explicit opt-outs (csrf_exempt(prefix), without_csrf()); the 0.8 service template and id_effect_kit web bundle use it (wave 3); serve()/server_layer behaviour is unchanged. Tests recommended_layer_blocks_post_without_token, recommended_layer_allows_exempt_prefix.'
  - 'CORS (#3): CorsConfig building tower_http::cors::CorsLayer; allow-credentials with a wildcard origin is a typed build error. CSP (#5): full directive API, per-request nonce exposed as CspNonce extractor, script hash directive. Headers (#6): HSTS, X-Frame-Options DENY, nosniff, Referrer-Policy strict-origin-when-cross-origin set if not present. Cookies (#7): CookieDefaults (HttpOnly, Secure, SameSite=Lax) and signed/private jars via cookie 0.18 with the key provided as an effect Layer. Each with tests named in the release research (cors_*, csp_*, default_security_headers_present, handler_header_overrides_default, cookie_*).'
  - 'examples/040_app_host.rs migrated to the new API and still runs; book part6/ch30-00-application.md security section rewritten (no false double-submit claim); docs/MIGRATION-0.8.md entry and CHANGELOG entry; tier-conformance and clippy/docs/coverage gates green for id_effect_axum.'
non_goals:
  - Server-side sessions and authentication (E5.2, 0.9).
  - Changing examples/http-showcase (owned by a concurrent session).
---

# 0.8 E5.1 — web hardening

Moves scorecard row sec.web from 1/8 to at least 6/8 (#1, #2, #3, #5, #6, #7). New dependencies for
id_effect_axum, all established: subtle 2.6, rand 0.9, tower-http 0.6 (cors, set-header), cookie 0.18
(signed, private), base64 0.22. Release spec: `.maestro/specs/releases/0.8.0.md`.
