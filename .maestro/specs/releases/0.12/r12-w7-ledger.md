---
title: 'Wave 7 — The Ledger over RPC next to REST, TypeScript client in its tests: r12-w7-ledger'
slug: r12-w7-ledger
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'The Ledger exposes its accounts and entries API as a #[rpc::service] (queries and mutations with idempotency keys, one server stream for statements) that calls the same handlers as its REST routes; REST and OpenAPI stay (ADR 0014: REST + OpenAPI remains the public API style).'
  - 'Its tests use the generated TypeScript client under Deno for a round trip, and the Rust client for the rest; rpc.contract.json is checked in; id-effect rpc check runs in the reference-ledger CI job against the last tag.'
  - 'Prod posture still clean with zero accepted risks; every procedure declared public or guarded (access-report snapshot updated).'
  - 'Tests `ledger_rpc_and_rest_agree`, `ledger_ts_client_round_trip`, `ledger_rpc_procedures_all_declared`, `ledger_rpc_mutation_idempotent`, `ledger_prod_posture_is_clean`.'
  - 'Gate: `ID_EFFECT_REQUIRE_DOCKER=1 cargo nextest run -p ledger` (Docker and Postgres; never the ambient DATABASE_URL); CI job `reference-ledger` green'
  - 'Owns: `examples/reference/ledger/**`, `.github/workflows/ci.yml` (steps of the `reference-ledger` job only)'
---

# r12-w7-ledger

Leaf of `.maestro/specs/releases/0.12.0.md`, section "E12.5 — Consumers and proof". The release spec's
row is binding; this file exists so Maestro can track the leaf as its own task.

Wave 7. Blocked by: r12-w6-kit-template and r12-e12-4-rpc-check. Second production consumer of
id_effect_rpc (with the Relay), which the web.rpc 100 target needs.
