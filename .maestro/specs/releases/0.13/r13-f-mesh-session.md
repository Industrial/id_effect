---
title: 'T12 carry (F30, F34) — node session pump; mesh RPC trace context and liveness: r13-f-mesh-session'
slug: r13-f-mesh-session
mode: heavy
work_type: spec-slice
risk_class: high
version: 1
acceptance_criteria:
  - 'F30: crates/id_effect_node gains a `SessionPump` (src/session_pump.rs, over `Session` at src/session.rs:27) that runs a session''s ingress through the mesh''s verified path, with no `Mesh::handle_envelope_from` bypass (mesh.rs:751; the 0.12 F33 failure mode); `channels::attach_session` / `attach_session_rpc` (crates/id_effect_rpc/src/channels/backplane.rs:708,722) use it instead of running a private mesh.'
  - 'F34: `MeshTransport` (crates/id_effect_rpc/src/client/mesh.rs) propagates trace context like the 0.11 dist.remote#8 path; mesh streams send heartbeats and fail with a typed `PeerSilent` error when the peer is silent past a finite deadline (default 3 heartbeat intervals), not only on `peer_down`; client/mesh.rs reaches ≥ 96 % regions.'
  - 'Secure by default (ADR 0013 R8, R1): only verified envelopes are delivered (unverified dropped and counted); heartbeat interval and deadline finite. Tests `session_pump_delivers_verified_envelopes_only`, `session_pump_drops_unverified_envelope`.'
  - 'Functional tests `channels_over_node_session_pump`, `mesh_transport_carries_trace_context`, `mesh_stream_detects_silent_peer` (TestClock), `mesh_endpoint_restart_is_transparent_to_callers` (0.12, still green).'
  - 'Tier: runtime (id_effect_node, strict bar) and integration (id_effect_rpc). Coverage: `cargo llvm-cov nextest -p id_effect_node --all-features --summary-only --fail-under-regions 96` and the same for id_effect_rpc with `--features mesh,channels`.'
  - 'Gate: `cargo nextest run -p id_effect_node --all-features`; `cargo nextest run -p id_effect_rpc --features mesh,channels`'
  - 'Owns: `crates/id_effect_node/src/{session.rs,session_pump.rs}`, `crates/id_effect_node/tests/session_pump.rs`, `crates/id_effect_rpc/src/{channels/backplane.rs,client/mesh.rs,transport/mesh.rs}`, `crates/id_effect_rpc/tests/mesh_liveness.rs`'
---

# r13-f-mesh-session

Leaf of `.maestro/specs/releases/0.13.0.md`, section "T12 carry" (0.12 overlay F30 and F34). The release
spec's row is binding; this file exists so Maestro can track the leaf as its own task.

Wave 3. Blocked by: the v0.12.0 tag (node lib.rs line append-only). Blocks r13-f-relay-host.
