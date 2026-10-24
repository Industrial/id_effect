---
title: 'T12 carry (F40) — Rust WsClient over wss:// with postcard: r13-f-ws-client-tls'
slug: r13-f-ws-client-tls
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'F40: `WsClient` (crates/id_effect_rpc/src/client/ws.rs:85) dials `wss://` itself with a rustls connector on the platform roots (the same root source as the platform HTTP client), and negotiates the `idrpc.postcard.v1` subprotocol when feature `postcard` is on; `WsClient::over` stays for custom streams.'
  - 'crates/id_effect_rpc/Cargo.toml: the tungstenite rustls feature added without a second rustls or tungstenite (`cargo tree -d`).'
  - 'Secure by default (ADR 0013 R8): server certificates verified against the platform roots; no "accept invalid certificates" API exists. Test `ws_client_rejects_untrusted_server_cert`.'
  - 'Functional tests `ws_client_wss_round_trip`, `ws_client_postcard_round_trip`, `ws_client_json_still_default`.'
  - 'Tier: integration. Coverage: `cargo llvm-cov nextest -p id_effect_rpc --features ws,postcard --summary-only --fail-under-regions 96`.'
  - 'Gate: `cargo nextest run -p id_effect_rpc --features ws,postcard`; `cargo tree -d -p id_effect_rpc --all-features` shows one rustls and one tungstenite'
  - 'Owns: `crates/id_effect_rpc/{Cargo.toml,src/client/ws.rs}`, `crates/id_effect_rpc/tests/ws_client_tls.rs`'
---

# r13-f-ws-client-tls

Leaf of `.maestro/specs/releases/0.13.0.md`, section "T12 carry" (0.12 overlay F40). The release spec's
row is binding; this file exists so Maestro can track the leaf as its own task.

Wave 3. Blocked by: the v0.12.0 tag; the only 0.13 leaf that edits crates/id_effect_rpc/Cargo.toml.
Blocks r13-f-relay-host.
