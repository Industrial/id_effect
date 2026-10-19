---
title: 'Wave 6 — Kit features rpc / rpc-ws / http3 / webtransport, posture probes, service template with RPC and HTTP/3 on: r12-w6-kit-template'
slug: r12-w6-kit-template
mode: heavy
work_type: spec-slice
risk_class: high
version: 1
acceptance_criteria:
  - 'id_effect_kit features: rpc (id_effect_rpc + macros, HTTP transport, implies web), rpc-ws (WebSocket + SSE transports, subscriptions; with events also the events-backed subscriptions), http3 (axum http3 + tls; with http-client also the platform HTTP/3 client), webtransport (http3 + rpc-ws + WebTransport); cluster additionally turns on id_effect_rpc?/channels and ?/mesh; all four in full; prelude exposes rpc::service and the procedure attributes.'
  - 'Posture probes (kit posture::check): every procedure declares public or guarded, WebSocket/WebTransport origins configured, 0-RTT off, QUIC Retry on, HTTP/3 enabled only with TLS, no id-effect dev certificate in prod; tests listed below.'
  - 'service template: a sample #[rpc::service] next to the REST routes, HTTP/3 on by default (server.http3.enabled = true, D4) on the same port as TLS TCP, TCP always on; prod config requires certificate paths or ACME (or the flag off behind a load balancer); README and the zero-to-production tutorial explain the flag.'
  - 'id-effect dev creates a localhost dev CA and a 13-day ECDSA P-256 certificate (WebTransport serverCertificateHashes compatible), rotates it before expiry, prints trust instructions, and never runs in the prod profile.'
  - 'scripts/template-smoke.sh generates a service and its TypeScript client (id-effect rpc gen ts), runs the service and a Deno round trip over HTTP/3 and over TCP; scripts/check-dangerous.sh lists every new dangerous_* API.'
  - 'Tests `template_service_serves_rpc_over_h3_and_tcp`, `template_http3_disabled_by_one_flag`, `posture_prod_refuses_dev_certificate`, `posture_reports_rpc_probes`, `posture_fails_on_undeclared_procedure`, `dev_cert_valid_for_webtransport_hashes`.'
  - 'Gate: `cargo nextest run -p id_effect_kit -p id_effect_tool --all-features`; `bash scripts/template-smoke.sh`; `bash scripts/check-dangerous.sh`; `cargo test -p book-snippets` (tutorial)'
  - 'Owns: `crates/id_effect_kit/**`, `crates/id_effect_tool/templates/**`, `crates/id_effect_tool/src/dev.rs`, `crates/id_effect_tool/Cargo.toml` (rcgen for the dev certificate, after r12-e12-4-gen-ts), `crates/id_effect_tool/tests/dev_cert.rs`, `scripts/template-smoke.sh`, `scripts/check-dangerous.sh`, `crates/id_effect/book/src/tutorial-zero-to-production.md`'
---

# r12-w6-kit-template

Leaf of `.maestro/specs/releases/0.12.0.md`, section "E12.4 — Clients and contracts" (kit and
templates). The release spec's row is binding; this file exists so Maestro can track the leaf as its
own task.

Wave 6. Blocked by: r12-e12-3-webtransport, r12-e12-4-gen-ts, r12-e12-2-channels, r12-e12-2-mesh-rpc,
r12-e12-3-certs-acme and r12-e12-3-h3-server. r12-w7-relay and r12-w7-ledger build only through the
kit. D4 reads "the starter" as the service template plus the tutorial (no separate starter exists).
