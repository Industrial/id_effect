---
title: 'T12 carry (F5) — one rcgen across platform, node, axum, sql, tool and kit: r13-f-rcgen'
slug: r13-f-rcgen
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'F5: every manifest uses rcgen 0.14.x (one version, pinned in each manifest as today): crates/id_effect_{tool,kit,axum}/Cargo.toml (`=0.13.2` today), crates/id_effect_{sql,node,platform}/Cargo.toml (`0.13` today); `cargo tree -d --workspace --all-features` shows no second rcgen (nor the untrusted/synstructure duplicates it pulled).'
  - 'The 19 files that use rcgen are ported to the 0.14 API: crates/id_effect_platform/src/tls/{acme,certs}.rs, crates/id_effect_platform/src/http/h3.rs, crates/id_effect_axum/src/server/{connection,tls}.rs, crates/id_effect_tool/src/dev.rs, crates/id_effect_axum/examples/092_http3.rs, crates/id_effect_node/examples/134_mtls_cluster.rs, and the tests crates/id_effect_sql/tests/tls.rs, crates/id_effect_node/tests/{shared_endpoint,transport_tls,transport_tcp_tls,discovery_kubernetes}.rs, crates/id_effect_axum/tests/{http3,tls_h2}.rs, crates/id_effect_platform/tests/{http3_client,tls_reload,quic_endpoint}.rs, crates/id_effect_kit/tests/posture.rs.'
  - 'No public signature changes, or each change is in docs/MIGRATION-0.13.md; the 0.11 and 0.12 TLS tests (mtls_rejects_peer_without_client_cert, mtls_rejects_cert_from_other_ca, rotated_cert_used_for_new_connections, reload_swaps_identity_on_endpoint_and_tcp, dev_cert_valid_for_webtransport_hashes) pass unchanged.'
  - 'Tier: integration (platform, axum, sql, tool, kit) and runtime (node, strict bar). Secure by default: certificate generation keeps ECDSA P-256 and the 13-day dev lifetime; no new opt-out.'
  - 'Coverage: `cargo llvm-cov nextest -p <crate> --all-features --summary-only --fail-under-regions 96` for each of the six crates.'
  - 'Gate: `cargo tree -d --workspace --all-features` lists rcgen at most once; `cargo nextest run -p id_effect_platform -p id_effect_node -p id_effect_axum -p id_effect_sql -p id_effect_tool -p id_effect_kit --all-features`; `cargo deny check`'
  - 'Owns: the rcgen dependency lines of the six manifests above, the 19 listed `.rs` files (rcgen call sites only), `Cargo.lock`, `deny.toml` (a skip entry for rcgen, if any)'
---

# r13-f-rcgen

Leaf of `.maestro/specs/releases/0.13.0.md`, section "T12 carry" (0.12 overlay F5). The release spec's
row is binding; this file exists so Maestro can track the leaf as its own task.

Wave 0 head. Blocked by: the v0.12.0 tag. Must merge before any later leaf edits
`crates/id_effect_axum/src/server/{connection,tls}.rs` (r13-f-ext-connect) or the six manifests
(r13-e9-5-views takes axum `Cargo.toml` in wave 1).
