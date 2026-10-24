---
title: 'T12 carry (F59, F55, F56) — the Relay on the host HTTP/3 + WebTransport, kit only: r13-f-relay-host'
slug: r13-f-relay-host
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'F59 (0.13 part): examples/reference/relay serves HTTP/3 and WebTransport from the host entry points (r13-f-webtransport-host), with no private `serve_h3` endpoint and host h3 on.'
  - 'F55: the Relay depends on `id_effect_kit` only (uses the kit `id_effect_process` re-export from r13-w5-kit-template; the direct id_effect_process dependency is removed from examples/reference/relay/Cargo.toml).'
  - 'F56 / F40 / F30 adoption: tests use `HttpTransport::with_bearer` instead of the wrapped HTTP client, the Rust WebSocket client dials `wss://` (r13-f-ws-client-tls), and channels run over the node session pump (r13-f-mesh-session).'
  - 'The remaining F59 items (exercise server streams, client streams, SSE, postcard, batching and WebSocket session resume in the Relay) are recorded as 0.14 E13.2 work in the leaf evidence, not done here.'
  - 'Tests: every 0.12 `relay_*` test green; new `relay_uses_host_webtransport`, `relay_depends_on_kit_only` (cargo metadata check); prod posture clean with zero accepted risks.'
  - 'Tier: example. Secure by default: prod posture probe set unchanged and clean; no new opt-out.'
  - 'Gate: `ID_EFFECT_REQUIRE_DOCKER=1 cargo nextest run -p relay`; CI job `reference-relay`; `bash scripts/tier-conformance.sh`'
  - 'Owns: `examples/reference/relay/**`, the `reference-relay` steps of `.github/workflows/ci.yml` (rebased after r13-w6-pipeline adds its job)'
---

# r13-f-relay-host

Leaf of `.maestro/specs/releases/0.13.0.md`, section "T12 carry" (0.12 overlay F59 0.13 part, F55
Relay manifest, F56 adoption). The release spec's row is binding; this file exists so Maestro can
track the leaf as its own task.

Wave 6. Blocked by: r13-w5-kit-template, r13-f-webtransport-host, r13-f-ws-client-tls,
r13-f-client-batching, r13-f-mesh-session, r13-f-ext-connect. Runs beside r13-w6-pipeline.
