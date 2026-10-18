---
title: 'E6.5 part 2 — Fault injection: r10-e6-5b-faults'
slug: r10-e6-5b-faults
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - '`Fault::{delay, fail, timeout}` applied to `SqlClient` and `HttpClient` doubles; delays run on the virtual clock (`TestExecutor::advance`) (#2); `FaultPlan::seeded(seed)` gives the same schedule for the same seed (#7). Tests `same_seed_same_fault_schedule`, `delay_advances_virtual_clock_only`, `retry_policy_survives_injected_latency`.'
  - 'Gate: `cargo nextest run -p id_effect_test -p id_effect_sql --features id_effect_test/fault`'
  - 'Owns: `crates/id_effect_test/src/fault/**` (feature `fault`), `crates/id_effect_test/Cargo.toml`, `crates/id_effect_sql/src/client.rs` (`TestSqlClient` delay only)'
---

# r10-e6-5b-faults

Leaf of `.maestro/specs/releases/0.10.0.md`, section "E6.5 part 2 — Fault injection (M, wave 1) · test.chaos #2 #7 #8". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.
