---
title: 'E11.4 part 1 — Compile-fail security suite (≥ 15 cases): r11-e11-4-compile-fail'
slug: r11-e11-4-compile-fail
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'tests/security_compile_fail (unpublished, created by r11-t0-skeletons) runs trybuild over at least 15 cases, each file named after its rule or finding and listed in docs/security/compile-fail.md with its ADR 0013 rule: h8_route_without_access_decision, h8_undecided_endpoint_into_router, h8_guard_needs_route_guard_type, h8_public_and_guard_together, m16_query_with_string, m16_execute_with_string, m16_query_raw_without_capability, m16_raw_sql_capability_has_no_public_constructor, m13_secret_of_non_zeroize_type, l3_generic_secret_equality_removed, secret_is_not_serialize, r8_plaintext_tcp_without_feature, r10_accepted_risk_without_reason, m19_cross_tenant_without_opt_out, deny_deprecated_redaction_policy_new, deny_deprecated_route_unchecked (spares beyond 15 allowed).'
  - 'Every `.stderr` snapshot is checked in against the toolchain pinned in rust-toolchain.toml; cases that the API answers with a `help:` note keep it in the snapshot.'
  - 'Gate: `cargo nextest run -p security_compile_fail`; `ls tests/security_compile_fail/ui/*.rs | wc -l` ≥ 15'
  - 'Owns: `tests/security_compile_fail/**`, `docs/security/compile-fail.md`'
---

# r11-e11-4-compile-fail

Leaf of `.maestro/specs/releases/0.11.0.md`, section "E11.4 part 1, E1.9, E2.6". The release spec's
row is binding; this file exists so Maestro can track the leaf as its own task.

Wave 3. Blocked by: r11-sec-h8-routing, r11-sec-sql-redis, r11-sec-secrets, r11-sec-tenancy,
r11-sec-logging, r11-e8-3-tcp-tls and r11-w3-kit-template (the APIs the cases exercise).
