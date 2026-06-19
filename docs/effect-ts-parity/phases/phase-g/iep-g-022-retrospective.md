# iep-g-022 — Spike retrospective: keep / delete / externalize

**Status:** Completed (Phase G delivery)

## Outcome: **Keep (experimental)**

We **keep** the `id_effect_workflow` crate at **0.1.x** as an **experimental** building block:

- Demonstrates **append-only** completions and **restart resume** with real tests.
- Documents **when not** to use it (see mdBook chapter `ch07-12-durable-workflow.md`).

## Not claimed

- No cluster protocol, no cross-node replay, no managed visibility UI.

## Next steps (optional, separate tasks)

- **Stabilize (G3):** semver policy, schema versioning, migration notes.
- **Externalize:** If product standardizes on Temporal only, archive the crate or reduce it to examples — requires a follow-up ADR and CI cleanup.
