---
title: 'E12.1 — #[rpc::service] in id_effect_rpc_macros, Rust client and contract snapshot: r12-e12-1-macro'
slug: r12-e12-1-macro
mode: heavy
work_type: spec-slice
risk_class: high
version: 1
acceptance_criteria:
  - '#[rpc::service(version = N)] on a trait whose methods carry one of #[query] #[mutation] #[server_stream] #[client_stream] #[bidi] #[subscription] and one of #[public] / #[guard(G)] generates: the handler trait (each method an Effect<A, E, R>, so DI, typed errors and TestExecutor apply), the Rust client generic over the transport (web.rpc#3), Service::contract() and the router mount; input, output and error types must be HasSchema (an on_unimplemented message names the derive).'
  - 'D11: id_effect_rpc::contract::assert_snapshot keeps rpc.contract.json current (UPDATE_GOLDEN=1 rewrites, as id_effect::testing::golden); two service versions can be served side by side (web.rpc#6).'
  - 'trybuild compile-fail cases in crates/id_effect_rpc/tests/ui, each first error with a help: line (K4 style): procedure without access decision (ADR 0013 R1), two kinds on one method, non-HasSchema type, #[bidi] without a Duplex return, duplicate procedure names, missing version.'
  - 'Tests `generated_client_round_trip_every_kind`, `contract_snapshot_is_current`, `service_versions_served_side_by_side`, `guarded_procedure_runs_guard_before_handler`; book part6/ch33-04-service-macro.md.'
  - 'Gate: `cargo nextest run -p id_effect_rpc -p id_effect_rpc_macros --all-features`; `cargo test -p id_effect_rpc --test ui`; `bash scripts/tier-conformance.sh id_effect_rpc_macros`'
  - 'Owns: `crates/id_effect_rpc_macros/**`, `crates/id_effect_rpc/src/service.rs`, `crates/id_effect_rpc/tests/{ui/**,ui.rs,service_macro.rs}`, `crates/id_effect_rpc/rpc.contract.json`, `crates/id_effect/book/src/part6/ch33-04-service-macro.md`'
---

# r12-e12-1-macro

Leaf of `.maestro/specs/releases/0.12.0.md`, section "E12.1 — RPC". The release spec's row is binding;
this file exists so Maestro can track the leaf as its own task.

Wave 3. Blocked by: r12-e12-1-kinds (the kinds the macro targets) and r12-t0-skeletons (the macro
crate). r12-e12-4-openapi-asyncapi and r12-e12-4-gen-ts read the contract it produces. The compile-fail
case `r1_rpc_procedure_without_access_decision` in `tests/security_compile_fail` is added by
r12-e12-5-fuzz-security from this leaf's trybuild case.
