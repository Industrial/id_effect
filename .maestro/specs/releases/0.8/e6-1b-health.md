---
title: 0.8 E6.1b — aggregated health indicators, built-in checks, readiness flip on shutdown
slug: r08-e6-1b-health
mode: light
work_type: feature
risk_class: medium
version: 1
acceptance_criteria:
  - 'id_effect_axum::health gains HealthIndicator (name + effect returning HealthStatus Up|Down|Degraded with serde_json detail) and HealthRegistry; checks run concurrently with per-check timeouts; /ready returns JSON {status, checks:{name:{status,detail,duration_ms}}} with 200 or 503; a timed-out check reports Down (tests).'
  - 'Backward compatible: existing ready(ReadinessCheck) and observability routes tests (health.rs:117-196) pass unchanged; ReadinessCheck adapts into an indicator.'
  - 'Built-in checks: id_effect_sql::health::sql_ping() (SELECT 1 via SqlClient; Up/Down tested with TestSqlClient) and a public pg pool check with pool size/idle detail (pg feature test in crates/id_effect_sql/tests); a cache indicator over resource/cache.rs stats().'
  - 'Readiness flips to not-ready (reason shutting_down) first in all three shutdown paths (server/lifecycle.rs run_until_shutdown, serve_router graceful future, ServerHandle::shutdown) before drain; integration test tests/health.rs over a real socket shows /ready 503 while an in-flight request still completes.'
  - 'Runnable example crates/id_effect_axum/examples/071_health.rs and book page part6/ch27-02-health.md linked from SUMMARY.md (moves obs.health level tested -> documented); clippy/docs/coverage gates green.'
non_goals:
  - Broker health checks (no first-party broker crate yet).
  - Metrics (leaf r08-e6-1a-metrics).
---

# 0.8 E6.1b — health

Moves scorecard row obs.health checks #3, #4, #7 and its level (tested -> documented); target 7/8.
Release spec: `.maestro/specs/releases/0.8.0.md`.
