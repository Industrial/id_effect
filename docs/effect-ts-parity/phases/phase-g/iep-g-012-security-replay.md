# iep-g-012 — Security: idempotency keys, PII in logs, replay safety

**Status:** Adopted (Phase G)

## Idempotency

- The spike keys completions by **`(workflow_id, seq)`**. Callers must allocate `seq` deterministically for a logical workflow graph (e.g. monotonic integers).
- Duplicate `(workflow_id, seq)` attempts after success should **read** the stored JSON, not re-run compute (see `run_step_typed`).

## PII and sensitive payloads

- Persisted `output_json` is **opaque to the library** — never log raw payloads at `info` in shared infrastructure without redaction.
- Prefer **references** (opaque ids) over embedding secrets or tokens in step outputs.

## Replay safety

- **Replay reads cached outputs** — side-effecting closures must not run again when a step is resumed; tests assert this property.
- If on-disk JSON is **tampered** between runs, deserialization fails with `WorkflowError::Json` — treat as **fatal for that workflow instance** and alert; do not silently re-execute without an explicit policy (could double-charge or double-send).

## Threat model boundary

- The crate does **not** provide encryption at rest, multi-tenant isolation, or authz — those belong to the embedding application or external orchestrator.
