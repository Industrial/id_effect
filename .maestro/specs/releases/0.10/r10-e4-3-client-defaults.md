---
title: 'E4.3 — HTTP client defaults + tracing: r10-e4-3-client-defaults'
slug: r10-e4-3-client-defaults
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - '`RetryingHttpClient` decorator over a core `Schedule` per D10, installed by the provider layer (`:388-391`) and configured once (#4); `default_client` sets connect/total timeouts. Tests (wiremock) `retries_idempotent_get_then_succeeds`, `post_not_retried_by_default`, `retry_after_is_honoured`, `default_timeout_applies`.'
  - 'Gate: `cargo nextest run -p id_effect_platform --all-features`'
  - 'Owns: `crates/id_effect_platform/src/http/{mod.rs,retry.rs}`'
---

# r10-e4-3-client-defaults

Leaf of `.maestro/specs/releases/0.10.0.md`, section "E4.3 — HTTP client defaults + tracing (M, wave 1) · web.client #4 #6, obs.tracing #6 #7". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.
