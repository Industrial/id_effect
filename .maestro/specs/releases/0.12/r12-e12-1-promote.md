---
title: 'E12.1 — Promote id_effect_rpc to a published crate, module layout and design note: r12-e12-1-promote'
slug: r12-e12-1-promote
mode: heavy
work_type: spec-slice
risk_class: high
version: 1
acceptance_criteria:
  - 'crates/id_effect_rpc passes tier integration: publish = false removed, [package.metadata.id_effect] tier = "integration", #![deny(missing_docs)], README with a runnable rust fence, coverage at the QUALITY_BAR floor, test density ≥ 1 test per 80 lines; labs wording removed from book part2/ch07-12-rpc-boundaries.md and part6/ch29-00-api-boundaries.md; new chapter part6/ch33-00-one-call-surface.md (overview, links to ch33-01..12).'
  - 'Cargo.toml declares, once, every 0.12 feature and optional dependency later leaves need (client, postcard, ws, sse, events, mesh, channels, http3, webtransport, test-util; id_effect_rpc_macros, postcard, id_effect_events, id_effect_node, id_effect_axum features ws/http3/webtransport, the tungstenite major that axum ws uses), so no later leaf edits Cargo.toml.'
  - 'Module layout: contract (serde model of a service contract: services, versions, procedures, kinds, input/output/error JSON Schemas, access decision; D11), codec/{mod,json}.rs (Codec trait + JSON), server/{mod,dispatch,http}.rs, client/{mod,http}.rs, wire (envelope types). src/codegen.rs (trait stub strings) is removed. lib.rs becomes an append-only shared file for later leaves (one pub mod / pub use line each).'
  - 'H8: the /rpc mount is a declared typed route; the low-level RpcGroup requires .public() or .guard(g) per procedure (typestate as id_effect_axum routes) and an unknown tag answers 404 Problem.'
  - 'Design note .maestro/specs/releases/0.12/r12-rpc-design.md: contract model, the six kinds and their HTTP mapping, the WebSocket/WebTransport frame format (call id, kind, flags, cursor), the cursor format, the batch body, the error union encoding, codec negotiation names. It is reviewed (owner or coordinator) before r12-e12-1-kinds starts; later leaves follow it.'
  - 'Tests `rpc_unregistered_tag_is_404_problem`, `rpc_group_requires_access_decision`, `contract_json_round_trips`, plus the moved tests still green.'
  - 'Gate: `bash scripts/tier-conformance.sh id_effect_rpc`; `cargo nextest run -p id_effect_rpc --all-features`; `cargo test -p id_effect_rpc --doc`; `cargo clippy -p id_effect_rpc --all-targets --all-features -- -D warnings`'
  - 'Owns: `crates/id_effect_rpc/**` (all files in wave 1), `.maestro/specs/releases/0.12/r12-rpc-design.md`, `crates/id_effect/book/src/part2/ch07-12-rpc-boundaries.md`, `crates/id_effect/book/src/part6/ch29-00-api-boundaries.md`, `crates/id_effect/book/src/part6/ch33-00-one-call-surface.md`'
---

# r12-e12-1-promote

Leaf of `.maestro/specs/releases/0.12.0.md`, section "E12.1 — RPC". The release spec's row is binding;
this file exists so Maestro can track the leaf as its own task.

Wave 1. Blocked by: r12-t0-skeletons (the move) and 0.11 r11-sec-h8-routing (typestate routes).
r12-e12-1-kinds waits for this leaf and for the design note review. After this leaf merges, the
release spec "File ownership" table splits `crates/id_effect_rpc/src/**` between the wave 2–5 leaves.
