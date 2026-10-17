---
title: 'E4.1b — HTTP middleware set: r09-e4-1b-layers'
slug: r09-e4-1b-layers
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'First-party `CompressionLayer`, `RequestIdLayer` (propagates `x-request-id`, opens a tracing span with method/route/id), `TimeoutLayer` (→ 503 Problem), `BodyLimitLayer` (→ 413), `RateLimitLayer` over `id_effect_resilience::RateLimiter` (→ 429, store pluggable for Redis); `SecurityLayer::recommended` unchanged. One test per layer.'
  - 'Gate: `cargo nextest run -p id_effect_axum --all-features`'
  - 'Owns: `crates/id_effect_axum/src/middleware/{compression.rs,request_id.rs,limits.rs,rate_limit.rs}`'
---

# r09-e4-1b-layers

Leaf of `.maestro/specs/releases/0.9.0.md`, section "E4.1b — HTTP middleware set (M, wave 2) · web.middleware #4 #5 #8 (+#6), data.cache #7, web.server #7". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.
