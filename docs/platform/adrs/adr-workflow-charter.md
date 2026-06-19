# ADR — Workflow charter for `id_effect_workflow`

**Status:** Accepted (revised 2026-06-17)  
**Date:** 2026-06-19 (original); 2026-06-17 (duroxide production path)  
**Context:** Phase G (`@effect/cluster`, `@effect/workflow` parity) needs explicit success criteria before investing in distributed execution. See `docs/effect-ts-parity/phases/phase-g-cluster-workflow.md`. The fp-events initiative locks **duroxide-pg** as the production durable workflow store on the shared PostgreSQL pool.

## Decision

Ship **durable step logs** with two tiers:

1. **`id_effect_workflow`** — `StepJournal` trait + Effect providers.
2. **Production (PostgreSQL)** — **duroxide** + **duroxide-pg** via `DuroxideStepJournal` and `DuroxideWorkflowRuntime` on [`PgPoolKey`](../../../crates/id_effect_sql_pg/src/pool_key.rs). duroxide-pg migrations run idempotently at provider startup.
3. **Dev / tests only** — SQLite via feature **`memory`** (`DurableWorkflowLog`, bundled rusqlite).
4. **`id_effect_fsm::workflow`** — bridge typed FSM snapshots into any `impl StepJournal`.
5. **External orchestrators** (Temporal, Cadence, Hatchet) remain the recommended path for multi-region, multi-tenant cluster workflow — documented, not reimplemented.

Do **not** build peer-to-peer cluster membership or a Temporal competitor inside `id_effect`.

## Success criteria (charter)

| Criterion | Production target | Dev / tests |
|-----------|-------------------|-------------|
| Crash recovery | Process restart resumes from last completed `seq` on PG | SQLite `memory` feature |
| Idempotency | Re-running `run_step_typed` for completed `(workflow_id, seq)` returns cached JSON | Same contract on SQLite |
| State typing | Serde round-trip for step outputs; FSM snapshots via `id_effect_fsm` | Same |
| Storage | **duroxide-pg** on shared pool | SQLite behind `memory` only |
| Security | No PII in step names by convention; replay reads only own `workflow_id` | Multi-tenant ACL on journal rows out of scope |

## Options considered

### A — Full `@effect/cluster` parity (in-repo)

Build shard routing, entity mailboxes, and distributed workflow engine in Rust.

**Rejected:** Scope rivals a product company; violates non-goals in `platform-workflow-cluster` spec.

### B — SQLite durable log + FSM bridge (v1 spike)

Minimal spike hardened into `id_effect_workflow` + `step_durable` / `register_fsm`.

**Superseded for production:** SQLite retained behind `memory` feature only.

### C — duroxide-pg on shared pool (chosen production path)

Microsoft-maintained durable orchestration; sqlx PostgreSQL provider; fits `PgPoolKey` ecosystem.

**Accepted:** Production default; adapter crate isolates API churn.

### D — External orchestrator only (docs)

Recommend Temporal SDK / REST and close the epic with no crate.

**Partial:** Temporal remains documented external path in book ch32; in-crate facade stays thin.

## Distributed path

**`StepJournal` trait** and **`NetworkJournalStub`** document how a remote journal would satisfy the same `(workflow_id, seq)` contract. Production distributed workflow should prefer:

- **Temporal** (or similar) for long-running sagas across services, or
- **Transactional outbox** (`id_effect_jobs`) + idempotent consumers for event-driven choreography.

## Consequences

- `id_effect_workflow` default features: `memory` (SQLite). Production apps enable `duroxide`.
- duroxide schema ownership stays in duroxide-pg migrations; step-cache tables documented alongside.
- Book chapter 32 and FSM example `002_durable_door_fsm` document both `memory` and `--features duroxide` paths.
- Breaking semver when removing SQLite-as-default for production docs.

## References

- `.maestro/specs/platform-workflow-cluster.md`
- `.maestro/specs/fp-events.md`
- `crates/id_effect_workflow/README.md`
- `crates/id_effect_fsm/src/workflow.rs`
