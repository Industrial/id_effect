---
title: 'E12.3 — HTTP/3 deployment guide (load balancer and direct-to-app, Kamal and Helm): r12-e12-3-deploy-guide'
slug: r12-e12-3-deploy-guide
mode: heavy
work_type: spec-slice
risk_class: low
version: 1
acceptance_criteria:
  - 'Book part6/ch29-07-http3-deployment.md documents both topologies: (a) a load balancer terminating HTTP/3 at the edge (needs nothing from the app; keep or disable server.http3.enabled for internal hops) and (b) direct-to-app with TCP and UDP 443 open, TLS-ALPN-01 ACME, the Alt-Svc behaviour, firewall rules, health checks over TCP, and what happens when UDP is blocked (clients stay on HTTP/2).'
  - 'Examples docs/deploy/http3/kamal.yml (UDP port publish) and docs/deploy/http3/helm-values.yaml (a UDP service port next to TCP, a NetworkPolicy for UDP 443) with a README; every Rust snippet in the chapter compiles through examples/book-snippets (K5).'
  - 'The mesh section links the shared-endpoint rule (D7) and quic.mesh_port, and states that the service template never shares a port with a mesh.'
  - 'Gate: `cargo test -p book-snippets`; `bash scripts/check-workflows.sh`; `yq e . docs/deploy/http3/kamal.yml docs/deploy/http3/helm-values.yaml > /dev/null`'
  - 'Owns: `crates/id_effect/book/src/part6/ch29-07-http3-deployment.md`, `docs/deploy/http3/{kamal.yml,helm-values.yaml,README.md}`'
---

# r12-e12-3-deploy-guide

Leaf of `.maestro/specs/releases/0.12.0.md`, section "E12.3 — HTTP/3". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.

Wave 4. Blocked by: r12-e12-3-h3-server and r12-e12-3-certs-acme (the guide describes their config
keys). Docs only.
