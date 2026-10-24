---
title: 'T12 carry (F39) — control frames leave the WebSocket message rate and get their own cap: r13-f-ws-credit'
slug: r13-f-ws-credit
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'F39: crates/id_effect_axum/src/realtime/ws.rs gains a frame classification hook on `WsConfig` (data vs control) used by the per-socket limiter (`:861`); crates/id_effect_rpc/src/transport/ws.rs classifies `credit` and `ping` frames as control, so fast client streams are no longer throttled by their own credit frames.'
  - 'Secure by default (ADR 0013 R4): control frames have their own finite rate cap (default 4× `messages_per_second`), and exceeding it closes with 1008 as data does. Tests `r4_control_frames_have_own_cap`, existing `r4_ws_message_rate_closes_1008`.'
  - 'Functional tests `credit_frames_do_not_throttle_data`, `fast_client_stream_at_rate_limit_completes`.'
  - 'Tier: integration (axum, rpc). Coverage: `cargo llvm-cov nextest -p id_effect_axum --features ws --summary-only --fail-under-regions 96` and the same for id_effect_rpc with `--features ws`.'
  - 'Gate: `cargo nextest run -p id_effect_axum --features ws`; `cargo nextest run -p id_effect_rpc --features ws`'
  - 'Owns: `crates/id_effect_axum/src/realtime/ws.rs` (limiter and classification hook), `crates/id_effect_rpc/src/transport/ws.rs` (frame classification), `crates/id_effect_rpc/tests/ws_credit_rate.rs`'
---

# r13-f-ws-credit

Leaf of `.maestro/specs/releases/0.13.0.md`, section "T12 carry" (0.12 overlay F39). The release spec's
row is binding; this file exists so Maestro can track the leaf as its own task.

Wave 3. Blocked by: the v0.12.0 tag. Blocks r13-f-ext-connect (same axum file, next wave).
