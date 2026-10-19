---
title: 'E12.3 — Certificate reload on the shared endpoint and ACME (TLS-ALPN-01): r12-e12-3-certs-acme'
slug: r12-e12-3-certs-acme
mode: heavy
work_type: spec-slice
risk_class: high
version: 1
acceptance_criteria:
  - 'Reload: the CertReloader (moved from node in r12-e12-3-quic-endpoint) drives the QUIC endpoint and the TCP TLS listener from one Identity; new connections use the new certificate, existing ones are not cut; dist.tls#7 stays met for the mesh.'
  - 'D8: platform feature acme issues and renews certificates with instant-acme over TLS-ALPN-01 (the acme-tls/1 ALPN answered by the rustls certificate resolver), renewal at two thirds of the lifetime with jitter, the account key stored with mode 0600, the directory URL from config (Lets Encrypt staging by default in non-prod profiles); a failed order keeps the current certificate and is logged and counted; DNS-01 is a non-goal.'
  - 'Tests `reload_swaps_identity_on_endpoint_and_tcp`, `existing_connections_survive_reload`, `acme_issues_and_renews_against_pebble` (pinned Pebble image by digest, ID_EFFECT_REQUIRE_DOCKER=1), `acme_failure_keeps_current_cert`, `acme_account_key_mode_0600`; book part6/ch29-09-certificates-acme.md.'
  - 'Gate: `cargo nextest run -p id_effect_platform --features tls,acme`; `ID_EFFECT_REQUIRE_DOCKER=1 cargo nextest run -p id_effect_platform --features acme --test acme_pebble`'
  - 'Owns: `crates/id_effect_platform/src/tls/{reload.rs,acme.rs}`, `crates/id_effect_platform/tests/{tls_reload.rs,acme_pebble.rs}`, `crates/id_effect/book/src/part6/ch29-09-certificates-acme.md`'
---

# r12-e12-3-certs-acme

Leaf of `.maestro/specs/releases/0.12.0.md`, section "E12.3 — HTTP/3". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.

Wave 2. Blocked by: r12-e12-3-quic-endpoint. TLS-ALPN-01 on the TCP listener is exercised end to end
once r12-e12-3-tls-h2 has merged; until then the test drives the resolver directly. Third in the
scope-cut order (dist.tls#7 is already met by reload).
