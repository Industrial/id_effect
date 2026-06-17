# Feature Intake Guide

Before claiming a task or creating a plan, run `maestro intake --paths <paths> --json` to classify the work.

## Classification Decision Tree

```text
Start with IntakeResult and intendedPaths.

┌─ Any path under .maestro/ | policies/ | skills/ | hooks/?
│    └─ yes → harness-improvement
│
├─ multi-domain flag OR paths span 3+ top-level dirs?
│    └─ yes → initiative
│
├─ All paths are manifests / .github/** / root config?
│    └─ yes → maintenance
│
├─ None of the paths exist yet?
│    └─ yes → new-spec
│
├─ All paths share one src/features/<one>/ root?
│    └─ yes → spec-slice
│
└─ else → change-request
```

First-match wins. A path that lands in `.maestro/` is `harness-improvement` even if other paths in the same call look like a `spec-slice`.

> **Note for brownfield codebases**: `spec-slice` requires the `src/features/<name>/` layout. Projects that organize code as `src/<domain>/` (without `features/`) will get `change-request` instead — that's the intended fallback. The two work types share most lane-driven next-steps, so adoption doesn't require restructuring.

## Common Patterns

| User Request | Work Type | Rationale |
|---|---|---|
| "Add new API endpoint" | spec-slice | Extending existing API surface |
| "Fix bug in login" | change-request | Modifying existing behavior |
| "Update dependencies" | maintenance | Chore-type work |
| "New auth system" | initiative | Multi-domain, large scope |
| "Add risk policy" | harness-improvement | Harness modification |

## Acting on the result

The intake response includes `recommendedNextStep` (lane-derived) and `recommendedNextSteps` (work-type + lane derived). Prefer the latter — it's more specific.

`IntakeResult.harnessImpact` is `true` whenever any path falls under `.maestro/`, `policies/`, `skills/`, or `hooks/` — independent of the work type. When it's true, plan to record a `harness-delta` evidence row at task close.
