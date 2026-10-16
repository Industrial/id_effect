---
title: 0.8 E6.1a — Prometheus scrape endpoint, runtime metrics, automatic HTTP and DB metrics
slug: r08-e6-1a-metrics
mode: light
work_type: feature
risk_class: medium
version: 1
acceptance_criteria:
  - 'id_effect::observability::prometheus::{render, CONTENT_TYPE} (new, no dependency) renders a MetricRegistry snapshot in exposition format 0.0.4; a golden-string unit test pins counter (_total), gauge, histogram (cumulative le buckets ending +Inf, _sum, _count; MetricState buckets are not cumulative, metric.rs:572), summary (quantile labels), frequency (value label) and label-value escaping.'
  - 'id_effect_axum::metrics::{metrics_route, metrics_router} serves GET /metrics with content type text/plain; version=0.0.4 (tests/metrics.rs, over the router).'
  - 'Runtime metrics: fiber counters incremented in FiberContext::with_parent (runtime/fiber_context.rs:84) and decremented once in mark_done (:204); id_effect_fibers_live / id_effect_fibers_started_total / id_effect_executor_in_flight sampled at render time; a test forks N blocked fibers and sees live >= N, then back to baseline.'
  - 'http_metrics_layer() records http_server_requests_total{method,route,status} and http_server_request_duration_seconds{method,route} with route from MatchedPath; unmatched paths create no per-path series (test).'
  - 'id_effect_sql::metrics::InstrumentedSqlClient<C: SqlClient> records db_client_operations_total{op,outcome} and db_client_operation_duration_seconds{op} (test with TestSqlClient); pool gauges via the public PgSqlClient::pool() without editing pg/client.rs.'
  - 'Runnable example crates/id_effect_axum/examples/070_metrics.rs and book page part6/ch27-01-metrics.md linked from SUMMARY.md; clippy -D warnings, docs and coverage gates green for touched crates.'
non_goals:
  - Health indicators and readiness (leaf r08-e6-1b-health).
  - A prometheus/prometheus-client dependency.
---

# 0.8 E6.1a — metrics

Moves scorecard row obs.metrics checks #5, #6, #7 (5/8 -> 8/8 possible; target 7/8). Release spec:
`.maestro/specs/releases/0.8.0.md`. Shared append-only files with E6.1b: crate lib.rs re-exports,
book SUMMARY.md, CHANGELOG.md.
