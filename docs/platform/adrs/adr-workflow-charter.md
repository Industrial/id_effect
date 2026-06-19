# ADR — Workflow charter for `id_effect_workflow`

**Status:** Accepted  
**Date:** 2026-06-19  
**Context:** Phase G (`@effect/cluster`, `@effect/workflow` parity) needs explicit success criteria before investing in distributed execution. See `docs/effect-ts-parity/phases/phase-g-cluster-workflow.md`.

## Decision

Ship **in-process durable step logs** as the v1 platform workflow surface:

1. **`id_effect_workflow`** — append-only completed-step journal with resume semantics (SQLite file or in-memory).
2. **`id_effect_fsm::workflow`** — bridge typed FSM snapshots into the step log.
3. **External orchestrators** (Temporal, Cadence, Hatchet) remain the recommended path for multi-region, multi-tenant cluster workflow — documented, not reimplemented.

Do **not** build peer-to-peer cluster membership or a Temporal competitor inside `id_effect`.

## Success criteria (charter)

| Criterion | v1 target | Out of scope |
|-----------|-----------|--------------|
| Crash recovery | Single process restart resumes from last completed `seq` | Cross-host leader election |
| Idempotency | Re-running `run_step_typed` for completed `(workflow_id, seq)` returns cached JSON | Automatic activity heartbeats |
| State typing | Serde round-trip for step outputs; FSM snapshots via `id_effect_fsm` | Arbitrary binary blobs without schema |
| Storage | SQLite (bundled) for dev/single-node; journal trait for future backends | Managed cloud workflow SaaS |
| Security | No PII in step names by convention; replay reads only own `workflow_id` | Multi-tenant ACL on journal rows |

## Options considered

### A — Full `@effect/cluster` parity (in-repo)

Build shard routing, entity mailboxes, and distributed workflow engine in Rust.

**Rejected:** Scope rivals a product company; violates non-goals in `platform-workflow-cluster` spec.

### B — SQLite durable log + FSM bridge (chosen v1)

Minimal spike hardened into `id_effect_workflow` + `step_durable` / `register_fsm`.

**Accepted:** Matches Effect.ts research→spike→optional-product arc; CI stays green; demos prove resume.

### C — External orchestrator only (docs)

Recommend Temporal SDK / REST and close the epic with no crate.

**Deferred:** Spike B already landed; keep crate as optional integration point and document C for cluster-scale deployments.

## Distributed path (spike, not product)

Wave 1 adds a **`StepJournal` trait** and **`NetworkJournalStub`** documenting how a remote journal would satisfy the same `(workflow_id, seq)` contract. Production distributed workflow should prefer:

- **Temporal** (or similar) for long-running sagas across services, or
- **Transactional outbox** (`id_effect_jobs`) + idempotent consumers for event-driven choreography.

## Consequences

- `id_effect_workflow` semver treats journal schema as stable; breaking storage changes require migration ADR.
- Book chapter 32 and FSM example `002_durable_door_fsm` are the canonical onboarding path.
- Future ADR required before shipping a real Postgres/Redis journal driver or embedding a cluster runtime.

## References

- `.maestro/specs/platform-workflow-cluster.md`
- `crates/id_effect_workflow/README.md`
- `crates/id_effect_fsm/src/workflow.rs`
