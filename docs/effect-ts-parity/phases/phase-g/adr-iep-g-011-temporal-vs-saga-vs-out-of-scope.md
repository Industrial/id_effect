# ADR — iep-g-011: Temporal vs custom saga store vs “out of scope”

**Status:** Accepted  
**Context:** Effect.ts exposes `@effect/cluster` and `@effect/workflow`. Rust services often need **durable execution** but not always a bespoke orchestrator.

## Decision

1. **Default recommendation for long-lived, multi-tenant, operations-heavy workflows:** adopt an **external orchestrator** (e.g. Temporal, Cadence, managed Step Functions) — operations, visibility, and multi-writer semantics are solved problems there.
2. **In-repo spike (`id_effect_workflow`):** support **SQLite-backed** append-only completion records for **single-writer** or **externally coordinated** workflows (dev harnesses, edge agents, bootstrap tools).
3. **Custom saga table (Postgres):** viable when you already own the OLTP schema and can enforce idempotency keys and leasing in SQL; treat as **application architecture**, not a second mini-Temporal inside `id_effect`.

## Rationale

- Building full cluster workflow semantics in this repository duplicates mature systems and spreads security/replay complexity across all consumers.
- A **small, auditable** persistence layer proves the **resume** property and documents composition with `Effect`, without locking the ecosystem into a heavyweight runtime.

## Consequences

- Production multi-writer scenarios must **layer leasing** (DB row locks, advisory locks, or external orchestrator) above the spike API.
- Contributors should prefer **integration guides** over expanding the in-repo orchestrator without a new ADR.
