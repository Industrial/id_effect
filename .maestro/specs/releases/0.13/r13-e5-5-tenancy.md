---
title: 'E5.5 — Tenant resolution for sql and authz, and per-tenant connection routing: r13-e5-5-tenancy'
slug: r13-e5-5-tenancy
mode: heavy
work_type: spec-slice
risk_class: high
version: 1
acceptance_criteria:
  - 'D10: `id_effect_authz::tenant::TenantResolver` resolves the tenant from the authenticated principal (`Tenanted`, crates/id_effect_authz/src/resource.rs:48) by default, from the Host header through a configured allowlist, or from a header only with `dangerous_tenant_from_header()`; an axum layer behind the existing authz `axum` feature provides the resolved `TenantId` to handlers and `TenantScope`.'
  - 'eco.tenancy #8 (margin): `id_effect_sql::TenantRouter`, an `SqlClient` mapping tenant → pool (configured map or a resolver effect), bounded LRU of pools (default 64), composable with `RoutingSqlClient` replicas (crates/id_effect_sql/src/routing.rs).'
  - 'Secure by default (ADR 0013 R1, R4, R10): unknown tenant has no connection (fails closed); no tenant means no tenant-scoped query (existing CurrentTenant rule); header resolution is a recorded opt-out; Host resolution outside the allowlist yields no tenant. Tests `tenant_resolved_from_principal_by_default`, `dangerous_tenant_from_header_is_recorded`, `host_outside_allowlist_has_no_tenant`, `unknown_tenant_has_no_connection`, `r4_tenant_pool_count_bounded`.'
  - 'Functional tests `tenant_router_composes_with_replicas`, `tenant_router_routes_two_tenants_to_two_databases` (`<pg>`, one container, two databases).'
  - 'Book `part6/ch30-07-audit-tenancy.md` gains sections "Resolving the tenant" and "One database per tenant"; the 0.9 sentence "Per-tenant connection routing is not part of 0.9" is replaced.'
  - 'The bridge from `TenantId` to `id_effect_sql::CurrentTenant` lives in kit feature `tenancy` (r13-w5-kit-template), so sql and authz gain no dependency on each other.'
  - 'Tier: integration (authz, sql). Coverage: `cargo llvm-cov nextest -p id_effect_authz --all-features --summary-only --fail-under-regions 96` and the same for id_effect_sql with `--features pg,sqlite,repository`.'
  - 'Gate: `cargo nextest run -p id_effect_authz --all-features --test tenant_resolver`; `ID_EFFECT_REQUIRE_PG=1 cargo nextest run -p id_effect_sql --features pg --test tenant_routing`; `bash scripts/check-dangerous.sh`'
  - 'Owns: `crates/id_effect_authz/src/tenant.rs`, `crates/id_effect_authz/tests/tenant_resolver.rs`, `crates/id_effect_sql/src/tenant_routing.rs`, `crates/id_effect_sql/tests/tenant_routing.rs`, `crates/id_effect/book/src/part6/ch30-07-audit-tenancy.md` (new sections)'
---

# r13-e5-5-tenancy

Leaf of `.maestro/specs/releases/0.13.0.md`, section "E5.5 — Tenancy helpers". The release spec's row
is binding; this file exists so Maestro can track the leaf as its own task.

Wave 2. Blocked by: r13-e3-3-relations (sql `lib.rs` is append-only after it). Blocks
r13-w5-kit-template. Fourth in the scope-cut order only for its #8 part (the resolver is never cut).
