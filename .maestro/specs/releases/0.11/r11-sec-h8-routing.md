---
title: 'E11.2 — Deny-by-default routing (H8): r11-sec-h8-routing'
slug: r11-sec-h8-routing
mode: heavy
work_type: spec-slice
risk_class: high
version: 1
acceptance_criteria:
  - 'H8 (0.11.0.md D8): typed routes carry an access marker; Route::public() or Route::guard(g: impl RouteGuard) is required before Router::route (crates/id_effect_axum/src/route/mod.rs:580) accepts the Endpoint; an undecided endpoint is a compile error. RouteGuard is an id_effect_axum trait implemented by id_effect_authz Guard (crates/id_effect_authz/src/guard.rs), so axum does not depend on authz.'
  - 'RouteInfo (route/registry.rs:96, #[non_exhaustive]) gains access: Public | Guarded(policies); Router::route_unchecked is the #[deprecated] one-minor shim (L9) and records a posture opt-out; Router::into_axum() installs a deny-by-default layer answering 404 Problem Details for any request whose matched path is not a registered public or guarded route (covers plain axum routes merged later) unless allow_unregistered_routes_dangerous(); Router::access_report() lists public routes and is logged at startup; OpenAPI marks security per route.'
  - 'Anonymous principals (crates/id_effect_auth/src/middleware.rs:10 continues as Anonymous; guard.rs:13-16) reach only public routes; the auth middleware docs say so.'
  - 'Every typed-route call site in the workspace is migrated in this leaf so `cargo build --workspace --all-targets` stays green: id_effect_axum examples and tests, id_effect_auth account and oidc routes, id_effect_console, examples/**, crates/id_effect_tool/templates/**, and book route snippets (K5).'
  - 'Regression tests `h8_route_without_access_decision_does_not_compile` (trybuild), `h8_route_unchecked_is_deprecated_and_recorded`, `h8_unregistered_axum_route_is_denied`, `h8_anonymous_reaches_only_public_routes`, `h8_access_report_lists_public_routes`. MIGRATION-0.11 section "H8 routing".'
  - 'Gate: `cargo nextest run -p id_effect_axum -p id_effect_authz -p id_effect_auth -p id_effect_console --all-features`; `cargo nextest run -p id_effect_axum --test ui`; `cargo build --workspace --all-targets`; `cargo test -p book-snippets` (K5 snippets compile)'
  - 'Owns: `crates/id_effect_axum/src/{route/mod.rs,route/registry.rs,route/access.rs,lib.rs}`, `crates/id_effect_axum/Cargo.toml`, `crates/id_effect_axum/tests/{route_access,ui,route_typed,route_registry,route_example,openapi}.rs`, `crates/id_effect_axum/tests/ui/**`, `crates/id_effect_axum/examples/**`, `crates/id_effect_authz/src/guard.rs`, `crates/id_effect_auth/src/{middleware.rs,account/**,oidc/**}` (route call sites), `crates/id_effect_console/src/**` (route call sites), route call sites only in `examples/**`, `crates/id_effect_tool/templates/**` and `crates/id_effect/book/src/**`'
---

# r11-sec-h8-routing

Leaf of `.maestro/specs/releases/0.11.0.md`, sections "E11.2 — Secure by default" and "Findings →
leaves" (H8). The release spec's row is binding; this file exists so Maestro can track the leaf as
its own task.

Wave 1. Blocked by: r11-sec-adr0013 (posture registry). This is the leaf most likely to be wrong
(0.11.0.md "Risks"); settle the typestate shape in a short design note in the PR description before
migrating call sites. The package name of the book snippets crate is to be confirmed by the
implementer (examples/book-snippets).
