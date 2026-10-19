---
title: 'T0 — ADR 0010 Amendment 8, move RPC out of labs, crate and Relay skeletons: r12-t0-skeletons'
slug: r12-t0-skeletons
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'ADR 0010 Amendment 8 (after Amendment 7, docs/adrs/0010-crate-tiers.md:220) names: id_effect_rpc (published, enters tier integration, moves to platform when the Relay depends on it; leaves the labs row); id_effect_rpc_macros (published proc-macro crate, tier integration); the id_effect_platform features tls, quic, http3, acme and the move of crates/id_effect_node/src/transport/certs.rs into id_effect_platform::tls with a one-minor deprecated re-export in node (rule 3, L9); examples/reference/relay (tier example, publish = false).'
  - '`git mv labs/id_effect_rpc crates/id_effect_rpc` (history kept); labs/Cargo.toml members lose id_effect_rpc; path dependencies rewritten (../../crates/x → ../x); the crate builds and its existing tests (tests/{codegen_stub,openapi_emit,wire_round_trip}.rs) pass unchanged; publish = false and tier labs stay until r12-e12-1-promote.'
  - 'crates/id_effect_rpc_macros skeleton: proc-macro = true, workspace version, README with a rust fence, #![deny(missing_docs)], one test; examples/reference/relay skeleton: Cargo.toml (package relay, publish = false, depends on id_effect_kit only), README, src/main.rs, one test.'
  - 'Root Cargo.toml members gain crates/id_effect_rpc, crates/id_effect_rpc_macros, examples/reference/relay; scripts/validate-crate-packaging.sh LEVEL/ORDER: --print-order lists id_effect_rpc_macros, then id_effect_rpc after id_effect_axum, id_effect_platform, id_effect_node and id_effect_events, and both before id_effect_kit and id_effect_tool; crates.io availability of both names recorded in the leaf evidence (generic User-Agent, no personal data); .github/workflows/ci.yml -p lines include both crates and the labs job no longer lists id_effect_rpc.'
  - 'docs/MIGRATION-0.12.md created with one heading per planned break (node transport::certs move, axum TLS/HTTP/3 config fields, HttpClientConfig::http3, QUIC mesh ALPN idm/1, labs RpcGroup::register → #[rpc::service], OpenAPI 3.0 → 3.1 output, template server.http3).'
  - 'Gate: `bash scripts/validate-crate-packaging.sh`; `bash scripts/validate-crate-packaging.sh --print-order`; `cargo nextest run -p id_effect_rpc -p id_effect_rpc_macros -p relay`; `cargo nextest run --manifest-path labs/Cargo.toml --workspace`'
  - 'Owns: `docs/adrs/0010-crate-tiers.md`, root `Cargo.toml` (members), `labs/Cargo.toml`, `crates/id_effect_rpc/**` (move only; r12-e12-1-promote owns it next), `crates/id_effect_rpc_macros/**` (skeleton; r12-e12-1-macro next), `examples/reference/relay/**` (skeleton; r12-w7-relay next), `scripts/validate-crate-packaging.sh`, `.github/workflows/ci.yml` (-p lines, labs job), `docs/MIGRATION-0.12.md` (headings)'
---

# r12-t0-skeletons

Leaf of `.maestro/specs/releases/0.12.0.md`, section "T0". The release spec's row is binding; this
file exists so Maestro can track the leaf as its own task.

Wave 0 head (day 0, alongside r12-t0-adr0014). Blocked by: the 0.11.0 tag merged into
`release/0.12.0`. r12-e12-1-promote, r12-e12-1-macro and r12-w7-relay build on the skeletons.
