---
title: 'E12.4 — id-effect rpc gen ts: a typed TypeScript client with batching, transport choice and resume: r12-e12-4-gen-ts'
slug: r12-e12-4-gen-ts
mode: heavy
work_type: spec-slice
risk_class: high
version: 1
acceptance_criteria:
  - 'id-effect rpc gen ts --contract <rpc.contract.json> --out <dir> writes plain TypeScript with no runtime dependency: types from the contract (structs, tagged unions, Option as optional or null, Vec, maps, brands as branded types, dates as ISO strings), error unions as discriminated unions on _tag, one client class per service and version (web.rpc#3).'
  - 'Runtime behaviour of the generated client: calls made in one microtask go out as one batch; transport choice WebTransport, then WebSocket, then SSE + POST; reconnect with exponential backoff and jitter and resume from the last cursor (web.realtime#7); AbortSignal cancels a call; the x-rpc header is always sent.'
  - 'cli.rs gains the rpc subcommand with gen ts only (Command is #[non_exhaustive], crates/id_effect_tool/src/cli.rs:29, so a new Rpc variant is not a break); r12-e12-4-rpc-check adds the check arm, so no stub ships (QUALITY_BAR check S).'
  - 'D12: tests type-check with deno check and run deno test (pinned Deno >= 2.4 via the CI setup step pinned by SHA); no package.json, node_modules or npm lockfile is added.'
  - 'Tests `gen_ts_golden_output` (crates/id_effect_tool/tests/golden/rpc/**), `generated_client_type_checks`, `generated_client_round_trip_against_test_server`, `generated_client_batches_same_microtask_calls`, `generated_client_resumes_after_reconnect`; book part6/ch33-11-typescript-client.md.'
  - 'Gate: `cargo nextest run -p id_effect_tool --test rpc_gen_ts`; `deno check crates/id_effect_tool/tests/ts/*.ts`; `deno test crates/id_effect_tool/tests/ts/`'
  - 'Owns: `crates/id_effect_tool/src/{rpc/mod.rs,rpc/gen_ts.rs,rpc/ts/**,cli.rs}`, `crates/id_effect_tool/Cargo.toml` (id_effect_rpc dependency for the contract model), `crates/id_effect_tool/tests/{rpc_gen_ts.rs,golden/rpc/**,ts/**}`, `crates/id_effect/book/src/part6/ch33-11-typescript-client.md`'
---

# r12-e12-4-gen-ts

Leaf of `.maestro/specs/releases/0.12.0.md`, section "E12.4 — Clients and contracts". The release
spec's row is binding; this file exists so Maestro can track the leaf as its own task.

Wave 5, critical path. Blocked by: r12-e12-1-macro (contract), r12-e12-1-batching (batch body) and
r12-e12-2-bidi-resume (cursor protocol). r12-e12-4-rpc-check and r12-w6-kit-template follow.
