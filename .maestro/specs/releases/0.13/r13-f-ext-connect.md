---
title: 'T12 carry (F15) — RFC 8441 extended CONNECT: WebSockets over TLS HTTP/2: r13-f-ext-connect'
slug: r13-f-ext-connect
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'F15: the TLS HTTP/2 server (crates/id_effect_axum/src/server/{connection,tls}.rs) enables the extended CONNECT protocol (SETTINGS_ENABLE_CONNECT_PROTOCOL); realtime/ws.rs accepts the h2 `CONNECT :protocol websocket` upgrade alongside the HTTP/1.1 upgrade, on the same typed routes.'
  - 'Secure by default (ADR 0013 R9, R4): the Origin allowlist, auth on connect and every 0.12 WebSocket limit (frame size, message and control rates, idle and heartbeat) apply to h2 WebSockets exactly as to h1; the 0.12 h2 concurrent-stream cap still bounds them. Tests `r9_h2_extended_connect_foreign_origin_refused`, `h2_websocket_limits_match_h1`.'
  - 'Functional tests `websocket_over_h2_round_trip`, `h1_websocket_unchanged`; the RPC WebSocket transport works over h2 (`rpc_over_h2_websocket`).'
  - 'Tier: integration. Coverage: `cargo llvm-cov nextest -p id_effect_axum --features tls,ws --summary-only --fail-under-regions 96`.'
  - 'Gate: `cargo nextest run -p id_effect_axum --features tls,ws`; `cargo nextest run -p id_effect_rpc --features ws`'
  - 'Owns: `crates/id_effect_axum/src/server/{connection.rs,tls.rs}` (h2 builder), `crates/id_effect_axum/src/realtime/ws.rs` (upgrade path only), `crates/id_effect_axum/tests/h2_websocket.rs`'
---

# r13-f-ext-connect

Leaf of `.maestro/specs/releases/0.13.0.md`, section "T12 carry" (0.12 overlay F15). The release spec's
row is binding; this file exists so Maestro can track the leaf as its own task.

Wave 4. Blocked by: r13-f-ws-credit (realtime/ws.rs) and r13-f-rcgen (connection.rs, tls.rs). Runs
beside r13-f-webtransport-host (different server files). In the scope-cut order (moves no row).
