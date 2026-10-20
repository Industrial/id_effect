---
title: 'Follow-up — HTTP/3 platform tuning: congestion control, UDP buffers, platform verifier, provided identity: r12-f-h3-platform'
slug: r12-f-h3-platform
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'F22 (D9 tuning task): `QuicLimits` (or a sibling transport setting) gains a congestion-control choice (`Cubic`, `Bbr`); the `h3` route defaults to BBR, the mesh `idm/1` route keeps Cubic; the choice is documented as quinn-experimental for BBR and is configurable per route. Test `h3_route_uses_bbr_mesh_uses_cubic`.'
  - 'F23: the QUIC endpoint sets UDP send/receive socket buffers (default 4 MiB, best effort, logs the granted size at debug); configurable; test asserts the requested size is applied or the shortfall logged.'
  - 'F10: the HTTP/3 client trusts the platform verifier (`rustls-platform-verifier =0.7.1`, already in Cargo.lock) when no explicit roots are configured, keeping `HttpClientConfig::root_certificates` as the override; SSRF and body-cap tests stay green.'
  - 'F24: `HostTls::Provided` can carry an `Identity` (or exposes one from its resolver) so a host with provided TLS serves HTTP/3 too; when it cannot, the existing log line stays. Test `provided_tls_serves_h3`.'
  - 'Gate: `cargo nextest run -p id_effect_platform --all-features`, `cargo nextest run -p id_effect_axum --features http3,tls`, clippy -D warnings, coverage of new code >= 96% regions, `cargo tree -d` one quinn / rustls.'
  - 'Owns: `crates/id_effect_platform/{Cargo.toml,src/quic/**,src/http/h3.rs,tests/http3_client.rs}`, `crates/id_effect_axum/src/server/{bootstrap.rs,tls.rs,lifecycle.rs}` (Provided identity only), CHANGELOG/MIGRATION-0.12 own hunks.'
---

# r12-f-h3-platform

Coordinator follow-up leaf of `.maestro/specs/releases/0.12.0.md` (E12.3), created from the
r12-e12-3-h3-server report (follow-ups F10, F22, F23, F24 in the execution overlay). The D9 decision
in F22 is binding: BBR is the one tuning task; r12-e12-5-benches confirms it on the real netem runner
or the pair gate becomes report-only.
