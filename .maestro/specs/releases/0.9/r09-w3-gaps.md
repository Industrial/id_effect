---
title: 'Close the small 0.9 level-audit gaps (R1-R3, H4, G2, G4)'
slug: r09-w3-gaps
mode: light
work_type: spec-slice
risk_class: low
version: 1
acceptance_criteria:
  - 'R1: book ch30-02-authorization.md gains a "data-layer scoping" section (Repository::for_current_tenant, the tenant filter on every statement, fail-closed) with a compiling snippet; cites the Ledger test organisations_never_see_each_other.'
  - 'R2: ch28-03-repositories.md shows a join, ordering and paging with sea-query + Page; crates/id_effect_sql/tests/query.rs gains a join test.'
  - 'R3: ch28-04-redis.md describes RedisKv / RedisHash / RedisJson as a typed key-value and JSON document store, with a compiling snippet.'
  - 'H4: crates/id_effect_sql/examples/020_typed_rows.rs (runs without a database: TestSqlClient or SQLite) showing SqlValue, params!, typed get, FromRow; [[example]] entry.'
  - 'G2 (eco.tenancy #7): a RateLimitKey keyed by the request tenant (from RequestCaps / a header / a closure over request extensions — whatever fits without id_effect_axum depending on id_effect_sql), with a test; ch30-07 no longer says per-tenant limits are not provided; ch29-03 mentions it.'
  - 'G4 (test.integration #8): a contract-testing helper in id_effect_test (feature http + openapi-aware: assert a TestResponse conforms to the response schema an OpenApi document declares for that operation and status), a test, a section in ch15-08, and use in Ledger tests/http.rs.'
  - 'All touched crates: nextest, clippy -D warnings, fmt; book_snippets doctests pass.'
---

# r09-w3-gaps

Small gaps from `.maestro/specs/releases/0.9/level-audit.md`. G1 (config change notification) and
G5 (two-level cache with pub/sub invalidation) are medium and move to 0.10; H1 (CI pg job features)
is done by the integrator; H2/H3 are evidence fixes at the blind re-score.
