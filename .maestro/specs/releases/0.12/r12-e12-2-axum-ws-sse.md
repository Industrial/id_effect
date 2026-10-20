---
title: 'E12.2 — WebSocket and SSE building blocks in id_effect_axum: r12-e12-2-axum-ws-sse'
slug: r12-e12-2-axum-ws-sse
mode: heavy
work_type: spec-slice
risk_class: high
version: 1
acceptance_criteria:
  - 'web.realtime#1: effect-native WebSocket endpoints (axum ws feature) on typed routes that declare public or guarded access (H8); the socket is a pull-based Stream plus a Sink inside a supervised fiber, so a dropped client interrupts its handler and runs finalizers.'
  - 'web.realtime#2: an SSE helper with event ids and Last-Event-ID resume; web.realtime#8: heartbeats (ping every 20 s, two missed pongs close the socket) and an idle timeout, on the executor clock (TestClock in tests).'
  - 'web.realtime#6 (connect half) and ADR 0013 R9: authentication on connect through the existing Authenticate layer; the Origin header is checked on upgrade against a configured allowlist, an empty list refuses every browser origin, and dangerous_any_origin() records a posture opt-out; the dev profile allows http://localhost:* by configuration.'
  - 'ADR 0013 R4: frame size 64 KiB (close 1009 above it), message rate per socket and sockets per client IP bounded by default (config).'
  - 'id_effect_axum/Cargo.toml declares, once, the features ws, sse, tls, http3, webtransport and their optional dependencies (tokio-rustls, hyper-util http2, h3 =0.0.8, h3-quinn =0.0.10, h3-webtransport =0.1.2, quinn, id_effect_platform/tls and /quic) for r12-e12-3-tls-h2, r12-e12-3-h3-server and r12-e12-3-webtransport.'
  - 'Tests `ws_round_trip_under_test_clock`, `sse_resumes_from_last_event_id`, `heartbeat_closes_dead_connection`, `dropped_socket_interrupts_handler`, `r9_ws_upgrade_from_foreign_origin_refused`, `r4_ws_frame_over_max_closes_1009`, `r4_ws_sockets_per_ip_capped`, `dangerous_any_origin_is_recorded`; examples 090_ws_echo.rs, 091_sse_ticker.rs; book part6/ch33-05-websocket-sse.md.'
  - 'Gate: `cargo nextest run -p id_effect_axum --all-features`; `cargo run -p id_effect_axum --features ws --example 090_ws_echo -- --self-test`; `bash scripts/check-dangerous.sh`'
  - 'Owns: `crates/id_effect_axum/Cargo.toml`, `crates/id_effect_axum/src/realtime/{mod.rs,ws.rs,sse.rs,origin.rs}`, `crates/id_effect_axum/tests/{ws.rs,sse.rs}`, `crates/id_effect_axum/examples/{090_ws_echo.rs,091_sse_ticker.rs}`, `crates/id_effect/book/src/part6/ch33-05-websocket-sse.md`'
---

# r12-e12-2-axum-ws-sse

Leaf of `.maestro/specs/releases/0.12.0.md`, section "E12.2 — Streams and realtime". The release
spec's row is binding; this file exists so Maestro can track the leaf as its own task.

Wave 1. Blocked by: r12-t0-skeletons (wave 0 head) and 0.11 r11-sec-limits / r11-sec-h8-routing.
Independent of the RPC crate, so it runs beside r12-e12-1-promote. r12-e12-2-rpc-ws-sse and
r12-e12-3-tls-h2 wait for it (the axum `Cargo.toml`).
