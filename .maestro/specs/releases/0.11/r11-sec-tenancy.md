---
title: 'E11.2 — Tenant isolation by default (M19): r11-sec-tenancy'
slug: r11-sec-tenancy
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'M19: when the request has a current tenant, resource policies built for a Tenanted resource (crates/id_effect_authz/src/resource.rs, Tenanted/same_tenant are opt-in today) include the tenant match automatically; crossing tenants needs cross_tenant_dangerous(reason), which records a posture opt-out; resources that are not Tenanted are unaffected.'
  - 'Regression tests `m19_tenanted_resource_requires_tenant_match_by_default`, `m19_cross_tenant_requires_named_opt_out`, `m19_untenanted_resource_unaffected`; book part6/ch30-07-audit-tenancy.md section.'
  - 'Gate: `cargo nextest run -p id_effect_authz --all-features`'
  - 'Owns: `crates/id_effect_authz/src/{resource.rs,policy.rs,lib.rs}`, `crates/id_effect_authz/tests/tenancy.rs`, `crates/id_effect/book/src/part6/ch30-07-audit-tenancy.md` (section)'
---

# r11-sec-tenancy

Leaf of `.maestro/specs/releases/0.11.0.md`, section "E11.2 — Secure by default" (M19). The release
spec's row is binding; this file exists so Maestro can track the leaf as its own task.

Wave 1. Blocked by: r11-sec-adr0013. SQL-level tenant scoping stays E5.5 (0.13).
