---
title: 'E11.2 — SSRF guard and no implicit redirects in the HTTP client: r11-sec-client-ssrf'
slug: r11-sec-client-ssrf
mode: heavy
work_type: spec-slice
risk_class: high
version: 1
acceptance_criteria:
  - 'M3 (0.11.0.md D10): HttpClientConfig::default() turns on an SsrfPolicy enforced by a custom reqwest DNS resolver that checks every resolved address (DNS-rebinding safe) and by a check on literal-IP URLs: loopback, RFC 1918, CGNAT 100.64/10, link-local 169.254/16 and fe80::/10 (incl. 169.254.169.254 and fd00:ec2::254), ULA fc00::/7, multicast, unspecified and IPv4-mapped forms are refused; schemes http and https only.'
  - 'Redirects off by default (crates/id_effect_platform/src/http/mod.rs:299-302 builds the client without a redirect policy today, so reqwest follows up to 10); follow_redirects(max) is opt-in and re-checks the policy on every hop; allow_hosts / allow_cidrs allowlist, settable from config; dangerous_allow_private_networks() records a posture opt-out. MIGRATION-0.11 section "SSRF default".'
  - 'Existing tests that call wiremock on 127.0.0.1 through HttpClient (id_effect_platform, id_effect_mail, id_effect_auth oidc, Ledger) use the allowlist; the Ledger test profile allowlists its fakes in config/test.toml.'
  - 'Regression tests `m3_ssrf_blocks_loopback_private_and_metadata`, `m3_ssrf_checks_every_resolved_address`, `m3_ipv4_mapped_ipv6_is_blocked`, `m3_redirects_not_followed_by_default`, `m3_followed_redirect_to_private_ip_is_blocked`, `m3_allowlist_permits_named_host`.'
  - 'Gate: `cargo nextest run -p id_effect_platform -p id_effect_mail -p id_effect_auth --all-features`; `ID_EFFECT_REQUIRE_DOCKER=1 cargo nextest run -p ledger` (Docker required)'
  - 'Owns: `crates/id_effect_platform/{src/http/**,Cargo.toml,tests/ssrf.rs}`, fixture lines in existing files under `crates/id_effect_mail/tests/**`, `crates/id_effect_auth/tests/**` and `examples/reference/ledger/tests/**`, `examples/reference/ledger/config/test.toml`'
---

# r11-sec-client-ssrf

Leaf of `.maestro/specs/releases/0.11.0.md`, section "E11.2 — Secure by default" (M3). The release
spec's row is binding; this file exists so Maestro can track the leaf as its own task.

Wave 2. Blocked by: 0.10.1 r101-client (http/** body cap), r11-sec-adr0013, and r11-sec-h8-routing
(owns Ledger test call sites in wave 1). JWKS fetching keeps its own client with the 0.10.1 M15
rules (https only, no redirects, 1 MiB cap).
