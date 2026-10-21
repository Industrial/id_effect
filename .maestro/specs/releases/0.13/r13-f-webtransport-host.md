---
title: 'T12 carry (F43, F45) — WebTransport in the host entry points; Deno test of the TS client over WebTransport: r13-f-webtransport-host'
slug: r13-f-webtransport-host
mode: heavy
work_type: spec-slice
risk_class: high
version: 1
acceptance_criteria:
  - 'F43: `ServerHandle` / `serve_router` (crates/id_effect_axum/src/server/{bootstrap,lifecycle}.rs) serve WebTransport on the host HTTP/3 endpoint when `server.http3.webtransport = true` (default false), using the 0.12 `webtransport::quic_limits` and `Http3Options::webtransport()` path from server/webtransport.rs; apps no longer need a private `serve_h3` endpoint.'
  - 'F45: crates/id_effect_tool/tests/ts/webtransport.test.ts runs the generated TypeScript client over WebTransport under the pinned Deno (flags such as `--unstable-net` recorded) against a test server with a 13-day ECDSA certificate and `serverCertificateHashes`; driven from crates/id_effect_tool/tests/rpc_gen_ts.rs. If the pinned Deno cannot open a WebTransport session, the leaf records the reason in its evidence, keeps the test `#[ignore]` with that reason, and the 0.12 Chromium browser test stays the evidence (release spec Risk 6).'
  - 'Secure by default (ADR 0013 R9, R4): WebTransport off unless configured; the CONNECT Origin check and the 0.12 limits apply on the host path. Tests `host_webtransport_off_by_default`, `r9_host_webtransport_foreign_origin_refused`.'
  - 'Functional tests `host_serves_webtransport_when_enabled`, `host_webtransport_resume_after_session_loss`, `generated_client_webtransport_round_trip_deno`.'
  - 'Tier: integration (axum, tool tests). Coverage: `cargo llvm-cov nextest -p id_effect_axum --features webtransport --summary-only --fail-under-regions 96` (lifecycle.rs stays in the documented coverage exclusion).'
  - 'Gate: `cargo nextest run -p id_effect_axum --features webtransport`; `cargo nextest run -p id_effect_tool --test rpc_gen_ts`'
  - 'Owns: `crates/id_effect_axum/src/server/{bootstrap.rs,lifecycle.rs,webtransport.rs}`, `crates/id_effect_axum/tests/host_webtransport.rs`, `crates/id_effect_tool/tests/ts/webtransport.test.ts`, `crates/id_effect_tool/tests/rpc_gen_ts.rs` (one new test)'
---

# r13-f-webtransport-host

Leaf of `.maestro/specs/releases/0.13.0.md`, section "T12 carry" (0.12 overlay F43 and F45). The release
spec's row is binding; this file exists so Maestro can track the leaf as its own task.

Wave 4. Blocked by: r13-f-h3-settings (server/h3.rs settled). Blocks r13-f-relay-host. The F45 part is
in the scope-cut order; F43 is not (the Relay depends on it).
