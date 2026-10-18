---
title: 'E8.3 — Encrypted TCP transport: r11-e8-3-tcp-tls'
slug: r11-e8-3-tcp-tls
mode: heavy
work_type: spec-slice
risk_class: high
version: 1
acceptance_criteria:
  - 'The TCP transport (plaintext today, crates/id_effect_node/src/transport/tcp.rs:1-5) carries TLS 1.3 with mutual authentication under feature tls-tcp, reusing transport/certs.rs (PEM loading and reload).'
  - 'Plaintext TCP exists only with feature insecure-plaintext-tcp and an explicit TcpTransport::dangerous_plaintext(), which records a posture opt-out (ADR 0013 R8); without the feature the plaintext constructor does not exist (r11-e11-4-compile-fail adds the case). MIGRATION-0.11 section "plaintext TCP feature".'
  - 'Tests `tcp_tls_mutual_auth_round_trip`, `tcp_tls_rejects_untrusted_peer`, `plaintext_tcp_opt_out_is_recorded`.'
  - 'Gate: `cargo nextest run -p id_effect_node --features tls-tcp`; `cargo nextest run -p id_effect_node --all-features`; `cargo build -p id_effect_node --no-default-features`'
  - 'Owns: `crates/id_effect_node/src/transport/{tcp.rs,tls_tcp.rs,mod.rs}`, `crates/id_effect_node/tests/transport_tcp_tls.rs`'
---

# r11-e8-3-tcp-tls

Leaf of `.maestro/specs/releases/0.11.0.md`, section "E8.3 — Mesh hardening". The release spec's row
is binding; this file exists so Maestro can track the leaf as its own task.

Wave 2. Blocked by: r11-e8-3-quic-mtls (certs.rs, transport/mod.rs, the Cargo.toml features).
