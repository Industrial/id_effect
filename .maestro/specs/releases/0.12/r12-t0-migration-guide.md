---
title: 'Exit — MIGRATION-0.12 and CHANGELOG: r12-t0-migration-guide'
slug: r12-t0-migration-guide
mode: heavy
work_type: spec-slice
risk_class: low
version: 1
acceptance_criteria:
  - 'docs/MIGRATION-0.12.md has one section per cargo semver-checks break with its shim and a before/after snippet: id_effect_node::transport::certs → id_effect_platform::tls (deprecated re-export for one minor); axum HostConfig / ConnectionLimits TLS and HTTP/3 fields; id_effect_platform HttpClientConfig::http3; QUIC mesh ALPN idm/1 (0.11 nodes are refused); labs RpcGroup::register → #[rpc::service] (and the low-level RpcGroup with .public()/.guard()); OpenAPI 3.0 → 3.1 output of id_effect_rpc; the service template config key server.http3.enabled.'
  - 'CHANGELOG.md 0.12.0 section with Added / Changed / Security / Scorecard subsections; the Scorecard section quotes REPORT-0.12.0 (E, RT, red rows, web.rpc, web.realtime, dist.tls); the Security section lists every call-surface default from the release spec Security table.'
  - 'Gate: `cargo semver-checks` output ⊆ the MIGRATION-0.12 list (diff recorded in the leaf evidence)'
  - 'Owns: `docs/MIGRATION-0.12.md`, `CHANGELOG.md`'
---

# r12-t0-migration-guide

Leaf of `.maestro/specs/releases/0.12.0.md`, section "T0" (exit). The release spec's row is binding;
this file exists so Maestro can track the leaf as its own task.

Exit. Blocked by: every implementation leaf and r12-w8-kpis. During the release both files are
append-only shared files (one section per leaf); this leaf makes them final.
