---
title: 'E12.2 — RPC over WebSocket and SSE (multiple transports): r12-e12-2-rpc-ws-sse'
slug: r12-e12-2-rpc-ws-sse
mode: heavy
work_type: spec-slice
risk_class: high
version: 1
acceptance_criteria:
  - 'web.rpc#5: every procedure kind over WebSocket frames multiplexed by call id (frame format from r12-rpc-design.md), subprotocols idrpc.json.v1 and idrpc.postcard.v1; subscriptions and server streams over SSE with mutations over POST next to it (the browser fallback).'
  - 'Backpressure: streams are pull-based end to end, so a slow client slows its own stream and the server buffers at most the configured window per stream; per-connection stream cap and frame cap (ADR 0013 R4).'
  - 'The Rust client chooses a transport (HTTP, WebSocket, SSE) from config; client/mod.rs holds the choice.'
  - 'Tests `every_kind_over_websocket`, `subscription_over_sse`, `mutation_over_post_next_to_sse`, `slow_ws_client_applies_backpressure`, `r4_ws_streams_per_connection_capped`, `ws_frame_decode_rejects_unknown_kind`; `transport::frame::decode` is public for the fuzz target; book part6/ch33-06-rpc-transports.md.'
  - 'Gate: `cargo nextest run -p id_effect_rpc --features ws,sse`; `cargo nextest run -p id_effect_rpc --all-features`'
  - 'Owns: `crates/id_effect_rpc/src/transport/{mod.rs,ws.rs,sse.rs,frame.rs}`, `crates/id_effect_rpc/src/client/{mod.rs,ws.rs,sse.rs}`, `crates/id_effect_rpc/src/server/mod.rs` (transport mounts, after r12-e12-1-kinds), `crates/id_effect_rpc/tests/{transport_ws.rs,transport_sse.rs}`, `crates/id_effect/book/src/part6/ch33-06-rpc-transports.md`'
---

# r12-e12-2-rpc-ws-sse

Leaf of `.maestro/specs/releases/0.12.0.md`, section "E12.2 — Streams and realtime". The release
spec's row is binding; this file exists so Maestro can track the leaf as its own task.

Wave 3, critical path (start it first in the wave). Blocked by: r12-e12-1-kinds and
r12-e12-2-axum-ws-sse. r12-e12-2-bidi-resume and r12-e12-5-benches (10,000-stream soak) follow.
