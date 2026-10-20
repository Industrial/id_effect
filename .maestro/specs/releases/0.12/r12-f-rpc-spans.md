---
title: 'Follow-up — per-call rpc.request spans on every transport; Relay WebTransport under its CSP: r12-f-rpc-spans'
slug: r12-f-rpc-spans
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'web.rpc#8: every call over WebSocket, WebTransport and SSE opens one `rpc.request` span (same fields as HTTP: rpc.system, rpc.service, rpc.method, correlation id, transport) parented on the connection span; the HTTP path gets a test. Tests `http_call_emits_rpc_request_span`, `ws_call_emits_rpc_request_span`, `sse_call_emits_rpc_request_span`, `webtransport_call_emits_rpc_request_span` (capturing subscriber, asserts one span per call with the tag).'
  - 'Relay CSP: determine why Chromium refused WebTransport with "violates the document''s Content Security Policy" in one loaded run (level audit, F59). If the Relay page CSP lacks the WebTransport origin in `connect-src` (https:// to the same host:port, which CSP treats as a different scheme from the page or a different port), fix the Relay page CSP (or SecurityLayer defaults if the default is wrong for WebTransport); add a test that the served CSP admits the WebTransport URL; rerun `ID_EFFECT_REQUIRE_BROWSER=1 cargo nextest run -p relay` 5 times. If it is a load artefact only, document the evidence.'
  - 'No change to security defaults beyond a correct CSP entry for the app''s own WebTransport endpoint.'
  - 'Gate: `cargo nextest run -p id_effect_rpc --all-features`, `-p relay` with ID_EFFECT_REQUIRE_DOCKER/DENO/BROWSER=1; clippy -D warnings; unary bench delta stays ≤ 3,000 ns (spans must not regress D5: rerun `rpc/unary_json` vs `rpc/axum_json_handler`).'
  - 'Owns: `crates/id_effect_rpc/src/transport/{ws,webtransport,sse}.rs` (span hunks), `crates/id_effect_rpc/src/span.rs`, `crates/id_effect_rpc/tests/spans.rs`, `examples/reference/relay/**` (CSP and the stale cursor comment at web/tests/relay.test.ts:92), CHANGELOG own hunk.'
---

# r12-f-rpc-spans

Coordinator follow-up leaf of `.maestro/specs/releases/0.12.0.md`, from the r12-w8-level-audit gaps 1 and 2
(.tmp/audit-scorecard-012.md).
