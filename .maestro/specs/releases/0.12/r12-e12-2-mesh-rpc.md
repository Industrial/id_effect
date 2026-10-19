---
title: 'E12.2 — RPC over the mesh: r12-e12-2-mesh-rpc'
slug: r12-e12-2-mesh-rpc
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'The same #[rpc::service] contract is callable node to node: an RPC endpoint process per service runs under a cluster-wide name (0.11 global registry), the client resolves it and calls over the mesh with postcard encoding; unary calls, server streams and two-way streams (credit-based) work.'
  - 'Spans: the caller fiber trace context becomes the parent of the handler span on the remote node (the 0.11 dist.remote#8 mechanism), and the rpc.request span carries the service, procedure and node.'
  - 'An endpoint process restart under its supervisor is transparent to callers (re-resolve on noproc, bounded retry for queries only).'
  - 'No edits to crates/id_effect_node (the endpoint is a Process behaviour registered through public node APIs).'
  - 'Tests `mesh_unary_and_stream_round_trip`, `mesh_call_carries_trace_context`, `mesh_endpoint_restart_is_transparent_to_callers`, `mesh_mutation_not_retried_on_noproc`; book part7/ch38-02-rpc-over-the-mesh.md.'
  - 'Gate: `cargo nextest run -p id_effect_rpc --features mesh`'
  - 'Owns: `crates/id_effect_rpc/src/{transport/mesh.rs,client/mesh.rs}`, `crates/id_effect_rpc/tests/mesh_rpc.rs`, `crates/id_effect/book/src/part7/ch38-02-rpc-over-the-mesh.md`'
---

# r12-e12-2-mesh-rpc

Leaf of `.maestro/specs/releases/0.12.0.md`, section "E12.2 — Streams and realtime". The release
spec's row is binding; this file exists so Maestro can track the leaf as its own task.

Wave 4. Blocked by: r12-e12-1-postcard (and through it r12-e12-1-kinds). The Relay uses it between
its nodes.
