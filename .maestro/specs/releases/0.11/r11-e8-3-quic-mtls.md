---
title: 'E8.3 — QUIC mutual TLS, PEM certificates and reload: r11-e8-3-quic-mtls'
slug: r11-e8-3-quic-mtls
mode: heavy
work_type: spec-slice
risk_class: high
version: 1
acceptance_criteria:
  - 'dist.tls#4: QUIC requires and verifies client certificates by default (replacing with_no_client_auth at crates/id_effect_node/src/transport/quic.rs:250, :254, :265); a peer without a client certificate, or with one from another CA, is refused.'
  - 'dist.tls#5: NodeCertificate::from_pem_files(chain, key) with PKCS#8, PKCS#1 and SEC1 keys and a CA bundle loader (today the only constructor is self_signed, quic.rs:50); dist.tls#7: a CertReloader polls the files (no notify crate, as 0.10 D4) and new connections use the new certificate without a restart; dist.tls#2: MeshConfig defaults to an encrypted transport (0.10.1 already refuses QUIC without roots or a pinned key).'
  - 'Every dangerous_* opt-out (accept any server cert, no client auth) calls id_effect::observability::posture::record_dangerous. id_effect_node/Cargo.toml declares, once, the features this release needs: quic, tls-tcp, insecure-plaintext-tcp, kubernetes and their deps (r11-e8-3-tcp-tls and r11-e8-3-k8s-discovery use them in wave 2 without editing Cargo.toml).'
  - 'Level tested → documented: book part7/ch38-01-transport-security.md, example 134_mtls_cluster.rs, integration test tests/transport_tls.rs. Tests `mtls_rejects_peer_without_client_cert`, `mtls_rejects_cert_from_other_ca`, `pem_chain_and_key_load`, `rotated_cert_used_for_new_connections`, `dangerous_accept_any_server_cert_is_recorded_in_posture`.'
  - 'Gate: `cargo nextest run -p id_effect_node --all-features`; `cargo run -p id_effect_node --features quic --example 134_mtls_cluster`; `cargo deny check`'
  - 'Owns: `crates/id_effect_node/src/transport/{quic.rs,certs.rs,mod.rs}`, `crates/id_effect_node/Cargo.toml`, `crates/id_effect_node/tests/transport_tls.rs`, `crates/id_effect_node/examples/134_mtls_cluster.rs`, `crates/id_effect/book/src/part7/ch38-01-transport-security.md`'
---

# r11-e8-3-quic-mtls

Leaf of `.maestro/specs/releases/0.11.0.md`, section "E8.3 — Mesh hardening". The release spec's row
is binding; this file exists so Maestro can track the leaf as its own task.

Wave 1. Blocked by: 0.10.1 r101-mesh (H3/H4/M9 changes to quic.rs, handshake.rs, session.rs) and
r11-sec-adr0013 (posture registry). r11-e8-3-tcp-tls and r11-e8-3-k8s-discovery follow in wave 2.
E8.3 is the rank-critical epic (single-epic cut −1.0 E).
