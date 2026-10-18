---
title: 'E4.3 — HTTP client defaults + tracing: r10-e4-3-tracing'
slug: r10-e4-3-tracing
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - '`OtelConfig.sampler` (always/never/ratio/parent-based) and `OTEL_TRACES_SAMPLER(_ARG)` (#7); `TracedHttpClient` (feature `platform`) opens a client span and calls `inject_into_http_request` (web.client#6, obs.tracing#6); `id_effect_sql` emits a `db.query` span with `db.system` and a parameter-free statement, bridged by `OtelSpanHook` (#6); `axum.rs` uses `.instrument(span)` instead of `span.enter()` across `.await`; `metrics.rs` doc links build without `pg`. Tests with the in-memory exporter `client_span_is_child_and_sends_traceparent`, `db_query_creates_span`, `ratio_sampler_from_env`, `server_span_not_held_across_await`. Book section in `ch27-00-observability.md`.'
  - 'Gate: `cargo nextest run -p id_effect_opentelemetry -p id_effect_sql --all-features`; `cargo doc -p id_effect_sql --no-deps` with `RUSTDOCFLAGS=-D warnings`'
  - 'Owns: `crates/id_effect_opentelemetry/src/{config.rs,otlp.rs,client.rs,axum.rs,span_bridge.rs}`, `crates/id_effect_opentelemetry/Cargo.toml`, `crates/id_effect_sql/src/metrics.rs`'
---

# r10-e4-3-tracing

Leaf of `.maestro/specs/releases/0.10.0.md`, section "E4.3 — HTTP client defaults + tracing (M, wave 1) · web.client #4 #6, obs.tracing #6 #7". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.
