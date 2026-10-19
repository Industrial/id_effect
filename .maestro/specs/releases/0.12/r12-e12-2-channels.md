---
title: 'E12.2 — The channels module: topics on mesh process groups, presence, authorization on join: r12-e12-2-channels'
slug: r12-e12-2-channels
mode: heavy
work_type: spec-slice
risk_class: high
version: 1
acceptance_criteria:
  - 'web.realtime#3: id_effect_rpc::channels topics with join and leave, mapped to mesh process groups (ProcessGroups / OrSwot, crates/id_effect_node/src/groups.rs:30,126) by one hub process per node; subscribers are RPC subscriptions (WebSocket, SSE, later WebTransport).'
  - 'web.realtime#5: a broadcast on any node reaches the subscribers on every node; delivery order per publisher is preserved; a node leaving removes its members.'
  - 'web.realtime#4: presence with per-connection metadata, join/leave diffs pushed to subscribers, convergence after a node goes down (monitor-driven cleanup).'
  - 'web.realtime#6 (join half) and ADR 0013 R1: joining a topic goes through a JoinPolicy; the default policy denies; an adapter runs an id_effect_authz policy without making id_effect_rpc depend on axum-only authz items.'
  - 'Tests `three_node_broadcast_under_test_clock` (in-memory mesh transport), `presence_converges_after_node_down`, `channel_join_denied_by_default_policy`, `leave_stops_delivery`, `publisher_order_preserved_per_topic`; example channels_chat.rs; book part6/ch33-09-channels-presence.md. No id_effect_channels crate (L15).'
  - 'Gate: `cargo nextest run -p id_effect_rpc --features channels`; `cargo run -p id_effect_rpc --features channels --example channels_chat -- --self-test`'
  - 'Owns: `crates/id_effect_rpc/src/channels/**`, `crates/id_effect_rpc/tests/{channels_three_nodes.rs,presence.rs}`, `crates/id_effect_rpc/examples/channels_chat.rs`, `crates/id_effect/book/src/part6/ch33-09-channels-presence.md`'
---

# r12-e12-2-channels

Leaf of `.maestro/specs/releases/0.12.0.md`, section "E12.2 — Streams and realtime". The release
spec's row is binding; this file exists so Maestro can track the leaf as its own task.

Wave 4. Blocked by: r12-e12-2-subscriptions (subscription kind on the wire) and the 0.11 global
registry / process groups. Moves four web.realtime checks; the Relay is its consumer. No edits to
`id_effect_node`: if a node API is missing, record it as a follow-up instead of editing node files
owned by nobody this wave.
