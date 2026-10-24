# Compile-fail security cases

The rules of [ADR 0013](../adrs/0013-secure-by-default.md) that the type system enforces have a
compile-fail case each: a small program that misuses the secure-by-default API and must be
rejected by the compiler (0.11.0, E11.4). The cases live in `tests/security_compile_fail/ui/`
(an unpublished crate, ADR 0010 Amendment 7) and run with [trybuild](https://docs.rs/trybuild);
each `.stderr` next to a case pins the diagnostic, including the `help:` and `note:` lines the
API adds to point at the safe call.

The cases go through `id_effect_kit` with the features `web`, `sql`, `cluster`, `authz` and
`logger`, as an app would; the 0.12 call-surface cases use `id_effect_rpc` and `id_effect_axum`
(feature `http3`) directly until the kit has its `rpc` and `http3` features; the 0.13 breadth cases
use `id_effect_admin` and `id_effect_ai` (feature `mcp-http`) directly, which the kit does not
re-export yet.

## Cases

| Case | Rule | Finding | What does not compile |
|---|---|---|---|
| `h8_route_without_access_decision` | R1 | H8 | `Router::route` on a typed route with neither `.public()` nor `.guard(g)` |
| `h8_undecided_endpoint_into_router` | R1 | H8 | `Router::route` on a plain Axum endpoint (`Endpoint::from_parts`) not yet decided |
| `h8_guard_needs_route_guard_type` | R1 | H8 | `.guard("admin")`: a guard is a `RouteGuard`, not a policy name |
| `h8_public_and_guard_together` | R1 | H8 | `.guard(g).public()`: a guarded route cannot be declared public afterwards |
| `deny_deprecated_route_unchecked` | R1 | H8 (L9 shim) | `Router::route_unchecked` under `#![deny(deprecated)]` |
| `m19_cross_tenant_without_opt_out` | R1 | M19 | a `Tenanted` resource declared of no tenant (`impl TenantScope for T {}`) to skip the tenant match |
| `m19_resource_without_tenant_scope` | R1 | M19 | `PoliciesBuilder::define` for a resource type that states no `TenantScope` |
| `m13_secret_of_non_zeroize_type` | R2 | M13 | `Secret::new("…")`: a `Secret<T>` needs `T: Zeroize` |
| `l3_generic_secret_equality_removed` | R2 | L3 | `==` on `Secret<u32>`: only `SecretString` / `SecretBytes` compare, in constant time |
| `secret_is_not_serialize` | R2 | M13 | `serde_json::to_string(&secret)`: `Secret` has no `Serialize` |
| `deny_deprecated_redaction_policy_new` | R3 | M11 | `RedactionPolicy::new()` (redacts nothing) under `#![deny(deprecated)]` |
| `m16_query_with_string` | R5 | M16 | `query` with a run-time `String` (`format!`): only `&'static str` converts to `Sql` |
| `m16_execute_with_string` | R5 | M16 | `execute` with a concatenated `String` |
| `m16_query_raw_without_capability` | R5 | M16 | `query_raw` in an environment without `RawSqlCapability` |
| `m16_raw_sql_capability_has_no_public_constructor` | R5, R10 | M16 | `RawSqlCapability { .. }`: only the recorded `RawSqlCapability::layer_dangerous` mints it |
| `r8_plaintext_tcp_without_feature` | R8 | dist.tls | `TcpTransport::dangerous_plaintext()` without id_effect_node's `insecure-plaintext-tcp` (the kit's `cluster` turns on `tls-tcp` only) |
| `r10_accepted_risk_without_reason` | R10 | every opt-out | `AcceptedRisks::accept(id)` without a reason |
| `r1_rpc_procedure_without_access_decision` | R1 | ADR 0014 C1 | an `#[rpc::service]` procedure with neither `#[public]` nor `#[guard(G)]` |
| `r10_zero_rtt_on_mutation` | R1, R10 | ADR 0014 (0-RTT replay) | enabling HTTP/3 0-RTT for mutations: the only opt-out is `Http3Options::dangerous_allow_zero_rtt_for_queries` (safe methods) |
| `r1_admin_site_without_guard` | R1, R7 | 0.13 admin (unguarded admin) | `AdminSite::router` before `.guard(g)` (an `AdminSite<Unguarded>` has no router), and `.guard(Public)`: the route-level `Public` marker is not a `RouteGuard`, so there is no public admin |
| `r1_mcp_endpoint_without_access_decision` | R1, R7 | 0.13 MCP (endpoint exposure) | `McpEndpoint::router` (the Streamable HTTP route) before `.guard(g)`, and `.guard(Public)` |

R4, R6, R7 and R9 are enforced at run time (defaults, regression tests and posture probes), not by
types; see the ADR's enforcement table.

## Running

```sh
cargo nextest run -p security_compile_fail
```

`tests/compile_fail.rs` also checks that there are at least 21 cases (0.13.0), that each has a `.stderr`
and that each is listed in the table above with its rule.

## Pinned toolchain

rustc diagnostics change between releases, so the snapshots are blessed on the compiler
`rust-toolchain.toml` pins (and CI uses). On another compiler the trybuild test is skipped with a
note; `UI_FORCE=1` runs it anyway. When the pin moves, re-bless:

```sh
TRYBUILD=overwrite cargo test -p security_compile_fail --test compile_fail
```

and review the `.stderr` diff: a case must still fail for its rule, not for an unrelated error.

## Adding a case

A new type-enforced rule, or a new way to break one, gets a case: `ui/<finding>_<what>.rs` with a
comment naming the rule, its blessed `.stderr`, and a row in the table above.
