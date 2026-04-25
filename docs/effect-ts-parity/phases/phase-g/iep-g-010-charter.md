# iep-g-010 — Charter: success criteria for workflow in this repo

**Status:** Adopted (Phase G)  
**Scope:** Single-process (or single-writer) **durable steps** with **crash/restart resume**, not a distributed cluster.

## In scope

1. **Durability:** Completed step outputs are persisted before the step is considered done, so a second process opening the same store can observe completion.
2. **Idempotency (per key):** `(workflow_id, seq)` uniquely identifies a step; re-entrancy returns the stored output without re-running side effects.
3. **Transparency:** Storage layout and limits are documented; callers understand this is **not** Temporal-class orchestration.
4. **Testability:** Restart behavior is covered by automated tests (see `id_effect_workflow` and `TESTING.md`).

## Out of scope (for this repo phase)

- Peer membership, leader election, or split-brain handling.
- Cross-region active/active execution.
- Automatic compensation / saga policy beyond what application code implements.

## Promotion criteria (G3)

If the spike is **kept**, stabilize semver, add explicit migration/versioning for the SQLite schema, and extend failure-injection tests. If **externalized**, replace crate guidance with integration ADR only and trim experimental surface area.
