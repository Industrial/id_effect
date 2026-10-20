---
title: 'E12.3 — WebTransport sessions carrying RPC two-way streams; browser tests: r12-e12-3-webtransport'
slug: r12-e12-3-webtransport
mode: heavy
work_type: spec-slice
risk_class: high
version: 1
acceptance_criteria:
  - 'axum feature webtransport: WebTransport sessions (h3-webtransport =0.1.2) on the HTTP/3 adapter; the extended CONNECT is a declared typed route (public or guarded) and its Origin header is checked against the same allowlist as WebSocket (ADR 0013 R9).'
  - 'id_effect_rpc feature webtransport: two-way streams and subscriptions over WebTransport bidirectional streams (one QUIC stream per call), resume after session loss with the r12-e12-2-bidi-resume cursor protocol; stream credits and session limits finite.'
  - 'D12 browser tests: Chromium driven from Rust over CDP (chromiumoxide or headless_chrome, choice recorded), using serverCertificateHashes with the 13-day ECDSA dev certificate; the client prefers WebTransport, then WebSocket. The browser test runs only with ID_EFFECT_REQUIRE_BROWSER=1 (fails, never skips, when set) and in the Relay CI job.'
  - 'Tests `webtransport_bidi_round_trip`, `webtransport_subscription_round_trip`, `webtransport_resume_after_session_loss`, `r9_webtransport_connect_from_foreign_origin_refused`, `browser_prefers_webtransport_then_websocket`; book part6/ch29-06-webtransport.md.'
  - 'Gate: `cargo nextest run -p id_effect_rpc -p id_effect_axum --features webtransport`; `ID_EFFECT_REQUIRE_BROWSER=1 cargo nextest run -p id_effect_rpc --features webtransport --test browser`'
  - 'Owns: `crates/id_effect_axum/src/server/webtransport.rs`, `crates/id_effect_rpc/src/transport/webtransport.rs`, `crates/id_effect_rpc/tests/{transport_webtransport.rs,browser.rs}`, `crates/id_effect/book/src/part6/ch29-06-webtransport.md`'
---

# r12-e12-3-webtransport

Leaf of `.maestro/specs/releases/0.12.0.md`, section "E12.3 — HTTP/3". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.

Wave 5. Blocked by: r12-e12-3-h3-server and r12-e12-2-bidi-resume. First in the scope-cut order: if it
slips, browsers use WebSocket and no scorecard check is lost. Open question 4 (Deno + CDP vs Node +
Playwright) applies here.
