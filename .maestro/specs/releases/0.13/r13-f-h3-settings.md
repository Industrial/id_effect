---
title: 'T12 carry (F25, F36) — real 0-RTT setting per route and a congestion-control switch for HTTP/3: r13-f-h3-settings'
slug: r13-f-h3-settings
mode: heavy
work_type: spec-slice
risk_class: high
version: 1
acceptance_criteria:
  - 'F25: the platform QUIC endpoint gains a per-route early-data setting (crates/id_effect_platform/src/quic/{config,router}.rs; early data is off at config.rs:220 today); the `h3` route can admit 0-RTT, the `idm/1` mesh route never can; `Http3Options::dangerous_allow_zero_rtt_for_queries` (crates/id_effect_axum/src/server/h3.rs:42) stops being a recorded no-op: it enables early data on the h3 route and admits only safe methods (GET/HEAD and RPC `query` procedures).'
  - 'F36: `Http3Options::congestion(CongestionControl::{Bbr, Cubic})` over `Route::congestion_control` (quic/router.rs:79, congestion.rs); default stays BBR on h3 (0.12 F22), mesh stays Cubic; config key `server.http3.congestion`.'
  - 'Secure by default (ADR 0013 R10, R1): 0-RTT off by default; the opt-out is recorded; a mutation in early data is refused with 425 Too Early. Tests `zero_rtt_off_by_default`, `zero_rtt_admits_only_safe_methods`, `zero_rtt_mutation_gets_425`, `mesh_route_never_accepts_early_data`; the 0.12 compile-fail case r10_zero_rtt_on_mutation still passes.'
  - 'Functional test `http3_congestion_switch_to_cubic`; the 0.12 h3 tests (`h3_serves_typed_routes_unchanged`, `alt_svc_header_on_tcp_responses`) unchanged.'
  - 'Tier: integration (platform, axum). Coverage: `cargo llvm-cov nextest -p id_effect_platform --features quic,http3 --summary-only --fail-under-regions 96` and the same for id_effect_axum with `--features http3`.'
  - 'Gate: `cargo nextest run -p id_effect_platform -p id_effect_axum --features http3`; `cargo nextest run -p security_compile_fail`; `bash scripts/check-dangerous.sh`'
  - 'Owns: `crates/id_effect_platform/src/quic/{config.rs,router.rs,congestion.rs}`, `crates/id_effect_axum/src/server/h3.rs`, `crates/id_effect_platform/tests/quic_early_data.rs`, `crates/id_effect_axum/tests/http3_settings.rs`'
---

# r13-f-h3-settings

Leaf of `.maestro/specs/releases/0.13.0.md`, section "T12 carry" (0.12 overlay F25 and F36). The release
spec's row is binding; this file exists so Maestro can track the leaf as its own task.

Wave 3. Blocked by: r13-f-rcgen (platform/axum TLS call sites settled). Blocks r13-f-webtransport-host.
F36 is built even if BBR passed the 0.12 netem gate: quinn marks BBR experimental and operators need a
switch without code.
