---
title: 'E12.3 — HTTP/3 server adapter into the axum Router, Alt-Svc, same-port TCP + UDP: r12-e12-3-h3-server'
slug: r12-e12-3-h3-server
mode: heavy
work_type: spec-slice
risk_class: high
version: 1
acceptance_criteria:
  - 'axum feature http3: an adapter feeds h3 (=0.0.8) + h3-quinn (=0.0.10) requests from the platform QuicEndpoint (ALPN h3) into the same axum Router as a tower Service, so every typed route, middleware, guard and RPC endpoint works over HTTP/3 unchanged; the adapter is one module (server/h3.rs) so the backend can be swapped once hyper ships HTTP/3.'
  - 'Alt-Svc: h3=":<port>"; ma=86400 is added to TLS TCP responses when HTTP/3 is enabled; TCP and UDP listen on the same port number; graceful shutdown drains both (GOAWAY on h3, the 0.11 drain on TCP); server.http3.enabled = false turns HTTP/3 and Alt-Svc off (one flag, D4).'
  - 'Limits mirror ConnectionLimits for HTTP/3 (connections, request header timeout, requests per connection, QUIC stream credits); the client IP comes from the QUIC peer address through the 0.10.1 trusted-proxy rules; 0-RTT off, dangerous_allow_zero_rtt_for_queries() allows it for safe methods only and records a posture opt-out.'
  - 'Spike first (D9): one HTTP/2 vs HTTP/3 p99 measurement under netem loss 1% delay 5ms with 64 multiplexed streams, recorded in the leaf evidence before the adapter is finished.'
  - 'Conformance: a CI job http3-conformance runs h3spec (release binary pinned by sha256) and curl --http3 (container image pinned by digest) against an example server; actions pinned by SHA (0.11 rule).'
  - 'Tests `h3_serves_typed_routes_unchanged`, `h3_middleware_and_guards_apply`, `alt_svc_header_on_tcp_responses`, `http3_disabled_by_one_flag`, `shutdown_drains_tcp_and_udp`, `zero_rtt_off_by_default`, `h3_client_ip_from_quic_peer`; book part6/ch29-05-http3.md.'
  - 'Gate: `cargo nextest run -p id_effect_axum --features http3`; `cargo nextest run -p id_effect_axum --all-features`; CI job `http3-conformance` green'
  - 'Owns: `crates/id_effect_axum/src/server/{h3.rs,alt_svc.rs,lifecycle.rs}`, `crates/id_effect_axum/tests/http3.rs`, `crates/id_effect_axum/examples/092_http3.rs`, `.github/workflows/ci.yml` (the `http3-conformance` job only), `crates/id_effect/book/src/part6/ch29-05-http3.md`'
---

# r12-e12-3-h3-server

Leaf of `.maestro/specs/releases/0.12.0.md`, section "E12.3 — HTTP/3". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.

Wave 3 (start first in the wave with r12-e12-2-rpc-ws-sse). Blocked by: r12-e12-3-tls-h2 (same
`server/` directory, TLS identity on TCP for Alt-Svc). r12-e12-3-webtransport,
r12-e12-3-deploy-guide and r12-e12-5-benches wait for it. If the spike shows HTTP/3 slower than
HTTP/2 under loss, raise release spec Open question 6 at once.
