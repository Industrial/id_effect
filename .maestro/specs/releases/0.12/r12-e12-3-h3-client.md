---
title: 'E12.3 — HTTP/3 client in id_effect_platform with Alt-Svc cache and HTTP/2 fallback: r12-e12-3-h3-client'
slug: r12-e12-3-h3-client
mode: heavy
work_type: spec-slice
risk_class: high
version: 1
acceptance_criteria:
  - 'HttpClientConfig::http3(bool) (opt-in for the client): requests go over h3 + h3-quinn when an Alt-Svc entry for the origin is known; the client never uses reqwest HTTP/3 and the workspace never sets --cfg reqwest_unstable.'
  - 'Alt-Svc cache: max-age honoured, bounded entries (LRU), cleared on a network-change error; fallback to HTTP/2 when the QUIC handshake does not complete within 300 ms, with a 5-minute negative cache per origin; the HTTP/2 request is never delayed by more than the fallback timer.'
  - 'The 0.11 SSRF guard (SsrfPolicy, crates/id_effect_platform/src/http/ssrf.rs) checks every resolved address on the h3 path too; timeouts, retries and the body cap (DEFAULT_MAX_BODY_BYTES, http/mod.rs:68) apply unchanged; TLS uses the platform webpki roots and verifies the server name.'
  - 'Tests `alt_svc_upgrades_next_request_to_h3`, `udp_blackhole_falls_back_to_h2_within_budget`, `negative_cache_skips_h3_after_failure`, `h3_path_enforces_ssrf_policy`, `h3_body_cap_enforced`, `h3_rejects_wrong_server_name`; book part6/ch29-08-http3-client.md.'
  - 'Gate: `cargo nextest run -p id_effect_platform --features http3`; `cargo nextest run -p id_effect_platform --all-features`; `! git grep -n reqwest_unstable -- ''*.toml'' ''*.rs'' ''*.yml'' ''*.nix''`'
  - 'Owns: `crates/id_effect_platform/src/http/{h3.rs,alt_svc.rs,mod.rs}`, `crates/id_effect_platform/tests/http3_client.rs`, `crates/id_effect/book/src/part6/ch29-08-http3-client.md`'
---

# r12-e12-3-h3-client

Leaf of `.maestro/specs/releases/0.12.0.md`, section "E12.3 — HTTP/3". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.

Wave 2. Blocked by: r12-e12-3-quic-endpoint (platform `Cargo.toml` and `quic`). The in-process tests
use a QUIC test server built from the platform endpoint, not the axum adapter, so this leaf does not
wait for r12-e12-3-h3-server. Edits `http/mod.rs`, which 0.11 r11-sec-client-ssrf owned.
