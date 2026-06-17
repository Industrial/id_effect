---
name: maestro
description: >-
  Run Maestro missions and tasks in this repo — spec validate, mission
  decompose, heavy-mode worktrees, parallel wave claims, verify/ship loop.
  Use when `.maestro/` exists, the user mentions maestro/mission/task/pln-/tsk-,
  or work is tracked under `.maestro/specs/` or `.maestro/missions/`.
---

# Maestro (project)

This repo is Maestro-initialized (`.maestro/MAESTRO.md`). Prefer **CLI** (`maestro …`) or **MCP** (`project-0-test-haskell-web-maestro`) for state changes; read skills under `~/.claude/skills/maestro-*` for full protocol.

## Read order

1. `.maestro/MAESTRO.md` — operational read order
2. `maestro status --json` — live missions/tasks
3. `.maestro/tasks/NOW.md` — human snapshot of in-flight work
4. Active mission sidecar + execution overlay (if present)

## Spec vs mission vs task

| Artifact | Path | Purpose |
|----------|------|---------|
| Product spec | `.maestro/specs/<slug>.md` | Acceptance criteria, `mode: heavy` for multi-PR |
| Mission record | `.maestro/missions/missions.jsonl` | `pln-…` id, links `spec_path` |
| Mission sidecar | `.maestro/missions/<slug>.md` | Verbatim plan / narrative (optional) |
| Execution overlay | `.maestro/missions/<slug>.execution.md` | Parallel waves, worktrees, subagents |
| Task | `.maestro/tasks/tasks.jsonl` | `tsk-…` — one PR per task (ADR-0006) |

## Heavy-mode loop (multi-PR)

```bash
maestro spec validate .maestro/specs/<slug>.md
maestro mission from-spec .maestro/specs/<slug>.md    # -> approved, pln-...
maestro mission decompose <pln-id> --file tasks.json  # -> planned + draft children
maestro mission show <pln-id>
maestro task claim <tsk-id> --agent <agent-id>        # auto worktree (heavy)
maestro task verify <tsk-id>
maestro task ship <tsk-id> [--pr-url <url>]
```

Light-mode: `maestro spec new` → grill via `maestro-design` → `maestro task from-spec` → claim (no mission).

## Parallel execution (this repo)

When `.maestro/missions/<slug>.execution.md` exists, **follow its wave table** — do not claim later-wave tasks until prior-wave tasks are **shipped**.

Pattern for parallel wave after foundation ships:

1. Ship wave-0 task and merge PR on main.
2. Launch **two** agents (Cursor subagents / worktrees), each:
   - `maestro task claim <tsk-a> --agent …`
   - `maestro task claim <tsk-b> --agent …`
3. Resolve PR conflicts by dependency order documented in the execution overlay.

Optional intra-task split while parent is claimed:

```bash
maestro task split <parent-tsk> --parallel "slice A" "slice B"
```

## Active mission: FSM workflow definitively

| Field | Value |
|-------|-------|
| Mission | `pln-mpsu3xxd-h0s6jn` |
| Spec | `.maestro/specs/fsm-workflow-definitively.md` |
| Plan (verbatim) | `.maestro/missions/fsm-workflow-definitively.md` |
| Parallelism | `.maestro/missions/fsm-workflow-definitively.execution.md` |

First claim: `tsk-mpsu3z87-xy3w58` (`domain-spec`).

## Elixir implementation coupling

Definitively code lives in `definitively/`. For domain/OTP work, also load:

- `.cursor/skills/elixir/elixir-core`
- `.cursor/skills/elixir/elixir-otp-design`
- `.cursor/skills/elixir/elixir-testing` (when editing `test/`)

Verify: `moon run definitively:format definitively:compile definitively:lint definitively:test` then `maestro task verify <tsk-id>`.

## Do not

- Hand-write heavy specs without acceptance criteria / `mode: heavy` when using `mission from-spec`.
- Claim multiple tasks on one agent unless the user directs it.
- Skip `verify` before `ship`.
- Encode FSM logic in CLI/MCP boundaries — keep orchestration in `Workflow.Engine` per the mission plan.

## See also

- `~/.claude/skills/maestro-mission/SKILL.md`
- `~/.claude/skills/maestro-task/SKILL.md`
- `~/.claude/skills/maestro-verify/SKILL.md`
- `~/.claude/skills/maestro-design/SKILL.md` — grill for new specs
