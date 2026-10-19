---
title: 'E12.3 — One QUIC endpoint with ALPN routing in id_effect_platform; mesh moves onto it: r12-e12-3-quic-endpoint'
slug: r12-e12-3-quic-endpoint
mode: heavy
work_type: spec-slice
risk_class: high
version: 1
acceptance_criteria:
  - 'D6: id_effect_platform features tls (certificates, Identity, rustls client/server configs, moved from crates/id_effect_node/src/transport/certs.rs:24-597 with history) and quic (QuicEndpoint with an AlpnRouter for h3 and idm/1); id_effect_node::transport::certs stays as a #[deprecated(note = "use id_effect_platform::tls")] re-export for one minor (L9); node features quic and tls-tcp depend on id_effect_platform with default-features = false; no dependency cycle (`cargo tree -i id_effect_node -p id_effect_platform` is empty).'
  - 'Endpoint defaults (ADR 0013 R4, R8): address validation (Retry) on, 0-RTT off, finite max concurrent bidi/uni streams, idle timeout, max UDP payload and receive window; each loosening is a dangerous_* opt-out recorded in the posture registry.'
  - 'D7: when idm/1 and h3 share one endpoint, client certificates are requested as optional with no CA hints, a presented certificate is still verified against the cluster CA, and the idm/1 handler refuses a connection without a verified client chain before reading any frame; quic.mesh_port (config) gives the mesh its own endpoint with handshake-level mutual TLS as in 0.11.'
  - 'The mesh QuicTransport runs on a QuicEndpoint (QuicTransport::on_endpoint) and requires ALPN idm/1; the 0.11 tests `mtls_rejects_peer_without_client_cert`, `mtls_rejects_cert_from_other_ca`, `pem_chain_and_key_load`, `rotated_cert_used_for_new_connections` pass on both shapes (dist.tls #2 #3 #4 #5 #7 kept).'
  - 'id_effect_platform/Cargo.toml declares, once, features tls, quic, http3, acme and their deps (rustls 0.23.46+, quinn 0.11.12+ on aws-lc-rs, rcgen, h3 =0.0.8, h3-quinn =0.0.10, instant-acme 0.8) for r12-e12-3-h3-client and r12-e12-3-certs-acme; one quinn and one rustls in `cargo tree -d`.'
  - 'Tests `alpn_routes_h3_and_idm`, `idm_without_client_cert_refused_on_shared_endpoint`, `idm_cert_from_other_ca_refused_on_shared_endpoint`, `unknown_alpn_refused`, `r4_quic_retry_on_by_default`, `r4_quic_limits_finite_by_default`, `mesh_port_splits_endpoint`, `deprecated_node_certs_path_still_compiles`; book part7/ch38-01-transport-security.md gains a "Shared endpoint" section.'
  - 'Gate: `cargo nextest run -p id_effect_platform -p id_effect_node --all-features`; `cargo run -p id_effect_node --features quic --example 134_mtls_cluster`; `cargo deny check`; `cargo tree -d -p id_effect_node --all-features` lists no second quinn or rustls'
  - 'Owns: `crates/id_effect_platform/Cargo.toml`, `crates/id_effect_platform/src/tls/{mod.rs,certs.rs}`, `crates/id_effect_platform/src/quic/**`, `crates/id_effect_platform/tests/quic_endpoint.rs`, `crates/id_effect_node/Cargo.toml`, `crates/id_effect_node/src/transport/{certs.rs,quic.rs,mod.rs}`, `crates/id_effect_node/tests/{transport_tls.rs,shared_endpoint.rs}`, `crates/id_effect/book/src/part7/ch38-01-transport-security.md` (section)'
---

# r12-e12-3-quic-endpoint

Leaf of `.maestro/specs/releases/0.12.0.md`, section "E12.3 — HTTP/3". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.

Wave 1. Blocked by: r12-t0-skeletons (Amendment 8 names the move) and the 0.11 tag (r11-e8-3-quic-mtls
and r11-e8-3-tcp-tls own these node files in 0.11). The leaf most likely to be wrong in this release
(see the release spec "Risks"). r12-e12-3-tls-h2, r12-e12-3-h3-client and r12-e12-3-certs-acme wait for
it. Open questions 3 and 5 in the release spec may change its crate placement or the default shape;
both are mechanical changes on top of this leaf.
