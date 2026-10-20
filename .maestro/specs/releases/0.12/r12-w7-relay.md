---
title: 'Wave 7 — Relay reference app: 3-node chat with presence over HTTP/3 and WebSocket: r12-w7-relay'
slug: r12-w7-relay
mode: heavy
work_type: spec-slice
risk_class: high
version: 1
acceptance_criteria:
  - 'examples/reference/relay is built only through id_effect_kit (features rpc, rpc-ws, http3, webtransport, cluster, mesh-mtls, events, sql-pg, auth, authz): chat rooms are channels topics, membership and presence per room, messages sent over a two-way RPC stream, room history as an events-backed subscription on Postgres, node-to-node calls over mesh RPC, three nodes on real QUIC (loopback ports, the shared endpoint of D7 with mutual TLS for idm/1).'
  - 'Clients in CI: the Rust client, the generated TypeScript client under Deno, and Chromium over CDP (WebTransport and WebSocket) with ID_EFFECT_REQUIRE_BROWSER=1.'
  - 'Prod posture clean with zero accepted risks; every procedure declared public or guarded; join denied without room membership; id-effect rpc check runs against rpc.contract.json at the last tag; the CI job reference-relay runs with Postgres and runs once more with UDP dropped by iptables to prove TCP fallback.'
  - 'Tier: the Relay depends on id_effect_rpc, so its tier line moves to platform (ADR 0012 rule 4, check P of scripts/tier-conformance.sh); web.rpc and web.realtime reach level production.'
  - 'Tests `relay_three_nodes_chat_over_http3`, `relay_three_nodes_chat_over_websocket`, `relay_presence_converges_after_node_loss`, `relay_resume_after_client_reconnect_no_gaps`, `relay_udp_blocked_falls_back_to_tcp`, `relay_join_denied_without_membership`, `relay_browser_chat_round_trip`, `relay_prod_posture_is_clean`, `relay_contract_check_passes`.'
  - 'Gate: `ID_EFFECT_REQUIRE_DOCKER=1 ID_EFFECT_REQUIRE_BROWSER=1 cargo nextest run -p relay`; `bash scripts/tier-conformance.sh` (all PASS); CI job `reference-relay` green'
  - 'Owns: `examples/reference/relay/**`, `.github/workflows/ci.yml` (the `reference-relay` job and its `ci-pass` needs entry), the tier line in `crates/id_effect_rpc/Cargo.toml`'
---

# r12-w7-relay

Leaf of `.maestro/specs/releases/0.12.0.md`, section "E12.5 — Consumers and proof" (the §6 Relay KPI).
The release spec's row is binding; this file exists so Maestro can track the leaf as its own task.

Wave 7. Blocked by: r12-w6-kit-template and r12-e12-4-rpc-check (and through them every E12.1–E12.4
leaf). Never cut. `ci.yml` is shared with r12-w7-ledger in this wave: different jobs, rebased in order
relay → ledger.
