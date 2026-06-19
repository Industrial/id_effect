# Harness

This document explains the **harness** — the development infrastructure layer that wraps the product code in this repository.

## Product Delta vs Harness Delta

Every change in this repo falls into one of two buckets, and many touch both.

**Product Delta** — changes that deliver user-facing value:

- New features, bug fixes, API endpoints
- UI components, business logic
- User-facing documentation

**Harness Delta** — changes that improve the development process itself:

- New validation rules, policy updates
- Workflow improvements, skill enhancements
- Process documentation, risk flags

A single PR can carry both. The `harness-delta` evidence kind exists so the harness improvements are not invisible — they show up alongside product evidence in the same task.

## Work Types

Six classifications, mutually exclusive. See `FEATURE_INTAKE.md` for the decision tree.

- `new-spec` — new feature with no existing implementation
- `spec-slice` — part of an existing spec or feature area
- `change-request` — modify, fix, or refine existing behavior
- `initiative` — large cross-domain work, multiple tasks
- `maintenance` — deps, configuration, tooling
- `harness-improvement` — improve the development harness itself

## How this fits the rest of the system

- `maestro intake` returns the work type as part of its result. Run it before claiming a task.
- The `harness-delta` evidence kind is recorded when a task closes and the work touched `.maestro/`, `policies/`, `skills/`, or `hooks/`.
- See `VALIDATION_LADDER.md` for how harness changes get verified.
