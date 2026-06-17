# Maestro Project State — Read Order

For any agent picking up work in this repo, read in order:

1. `.maestro/MAESTRO.md` (this file) — read order, lane policy, daily commands.
2. `AGENTS.md` (repo root) — code conventions, feature boundaries, build/test commands.
3. `.maestro/tasks/NOW.md` — what is currently in flight.
4. `maestro status --json` — live state across missions, tasks, pending loosenings.
5. `.maestro/policies/*.yaml` — risk, autopilot, release, sensitive-paths, owners.
6. `.maestro/specs/<id>/spec.json` — acceptance criteria for the active mission, if any.

If two sources conflict, the lower-numbered file is operational; the higher-numbered file is informational.

## Before code: run `maestro intake`

Pre-flight risk classification before writing code. `maestro intake --paths <paths> [--flag <flag> ...]` returns a lane (`tiny` | `normal` | `high-risk`), the derived risk class, and the recommended next step. Use it as the entry point for any non-trivial change.

- `tiny` — patch directly, run validation, close with reason.
- `normal` — `maestro spec new` then `maestro task from-spec`, then `maestro plan check`.
- `high-risk` — Spec acceptance criteria plus threat-model evidence required.

## Two outputs per task

Every task close should answer two questions:

1. **Product delta** — what changed in user-facing or product behavior?
2. **Harness delta** — what should we change so the next agent has it easier? (memory ratchet, skill update, `maestro doctor` finding, friction note in this file). Answer "none" if truly nothing.

If the harness delta is non-trivial, capture it before the close so the next session inherits it.

## Daily commands

```bash
maestro status --json                                 # what is in flight
maestro intake --paths <paths> [--flag <flag>]        # pre-code risk classifier
maestro task from-spec <path>                         # materialize a task from an authored spec
maestro mission decompose <pln-id> --file -           # heavy-mode: batch-create child tasks
maestro plan check --task <id> --plan-file <path>     # plan-time consistency check
maestro doctor                                        # scaffold + init.sh + verdict freshness
```
