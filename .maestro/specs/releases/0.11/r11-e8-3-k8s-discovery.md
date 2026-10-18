---
title: 'E8.3 — Kubernetes discovery: r11-e8-3-k8s-discovery'
slug: r11-e8-3-k8s-discovery
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'dist.membership#4 (0.11.0.md D18): KubernetesDiscovery under feature kubernetes lists the ready addresses of a headless service through the in-cluster EndpointSlice API over rustls, with the service-account token and CA from /var/run/secrets/kubernetes.io/serviceaccount and the namespace from its file; no kube-rs; implements the Discovery trait (crates/id_effect_node/src/discovery.rs:14) beside StaticDiscovery (:21) and DnsDiscovery (:43) and composes with ChainedDiscovery (:69).'
  - 'Tests against an in-process fake API server: `kubernetes_discovery_lists_ready_endpoints`, `kubernetes_discovery_skips_not_ready`, `kubernetes_discovery_uses_service_account_token`, `kubernetes_discovery_rejects_untrusted_api_cert`; book section in part7/ch38-00-distribution.md.'
  - 'Gate: `cargo nextest run -p id_effect_node --features kubernetes --test discovery_kubernetes`; `cargo nextest run -p id_effect_node --all-features`'
  - 'Owns: `crates/id_effect_node/src/discovery.rs`, `crates/id_effect_node/tests/discovery_kubernetes.rs`, `crates/id_effect/book/src/part7/ch38-00-distribution.md` (section)'
---

# r11-e8-3-k8s-discovery

Leaf of `.maestro/specs/releases/0.11.0.md`, section "E8.3 — Mesh hardening". The release spec's row
is binding; this file exists so Maestro can track the leaf as its own task.

Wave 2. Blocked by: r11-e8-3-quic-mtls (declares the `kubernetes` feature and its deps in
id_effect_node/Cargo.toml).
