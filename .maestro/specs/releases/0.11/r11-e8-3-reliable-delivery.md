---
title: 'E8.3 — At-least-once delivery and remote trace context: r11-e8-3-reliable-delivery'
slug: r11-e8-3-reliable-delivery
mode: heavy
work_type: spec-slice
risk_class: high
version: 1
acceptance_criteria:
  - 'At-least-once mode (0.11.0.md D19), opt-in per send (Addr::send_reliable) and per mesh (DeliveryMode::AtLeastOnce); at-most-once stays the default. Per-peer sequence numbers, ack frames, a bounded retransmit buffer (count and bytes, configurable) with backoff, and a receiver dedup window keyed by (incarnation, seq), so reconnects do not duplicate; messages pending when the peer restarts with a new incarnation fail with DeliveryFailed to the sender''s monitor (documented).'
  - 'PROTOCOL_VERSION bumps (crates/id_effect_node/src/wire.rs:16); older nodes are refused with VersionMismatch; MIGRATION-0.11 and CHANGELOG say so.'
  - 'dist.remote#8: the sending fiber''s trace context is carried on remote cast, call and send (today an empty TraceContext::new() at mesh.rs:369, :384, :424) and becomes the parent span on the receiver.'
  - 'Fuzz targets mesh_frame and mesh_envelope added to fuzz/ (counted in the E11.4 KPI).'
  - 'Tests `reliable_send_survives_connection_drop`, `duplicates_suppressed_after_reconnect`, `retransmit_buffer_bound_faults_oldest`, `at_most_once_remains_default`, `remote_call_carries_trace_context`, `version_mismatch_refused`.'
  - 'Gate: `cargo nextest run -p id_effect_node --all-features`; in fuzz/: `cargo +nightly-<pinned> fuzz build mesh_frame mesh_envelope` and each run `-max_total_time=60` with no crash'
  - 'Owns: `crates/id_effect_node/src/{envelope.rs,wire.rs,mesh.rs,session.rs,delivery.rs,lib.rs}`, `crates/id_effect_node/tests/reliable_delivery.rs`, `fuzz/fuzz_targets/{mesh_frame.rs,mesh_envelope.rs}` and their `fuzz/Cargo.toml` entries'
---

# r11-e8-3-reliable-delivery

Leaf of `.maestro/specs/releases/0.11.0.md`, section "E8.3 — Mesh hardening". The release spec's row
is binding; this file exists so Maestro can track the leaf as its own task.

Wave 2. Blocked by: r11-e8-3-global-registry (envelope.rs, mesh.rs, lib.rs) and r11-fuzz-ci (fuzz/
workspace). Builds on 0.10.1 r101-mesh session changes.
