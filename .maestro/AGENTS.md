# Maestro Project Bootstrap

This project uses Maestro as a long-running agent harness. This file is a TOC,
not an encyclopedia — read it as pointers and open the linked docs as needed.

## Where to read

| Topic | Doc |
|---|---|
| Read order, lane policy, daily commands | `.maestro/MAESTRO.md` |
| Harness positioning + principles | `docs/harness-positioning.md` |
| Verb reference | `docs/cli-reference.md` |
| Witness ladder + evidence kinds | `docs/witness-levels.md` |
| Risk classes + policy | `docs/risk-class-derivation.md`, `docs/policy-format.md` |
| Schedule recipes (external triggers only) | `docs/schedule-recipes.md` |
| Architecture lints | `docs/architecture-lints.md` |

## Layout

- `.maestro/bootstrap/` — committed bootstrap assets (`init.sh`, services, library, validation)
- `.maestro/skills/` — project-local agent skills
- `.maestro/missions/` — `<slug>.md` sidecars are tracked design intent; `missions.jsonl` and per-mission subdirs are runtime state
- `.maestro/sessions/` — runtime state (handoff packets live globally)
- `.maestro/tasks/contracts/` + `.maestro/tasks/contract-templates/` — versioned contracts and reusable drafts
- `skills/built-in/` — shipped built-in fallback skills

## Daily loop (one-liners)

- Pre-flight risk: `maestro intake --paths <paths>`
- Plan check: `maestro plan check --task <id> --plan-file <path>`
- Contract lifecycle: contracts are auto-created on `maestro task claim <id>`; inspect via `maestro contract show --task <id>` and amend via `maestro contract amend --task <id> --reason "..."`
- Verdict: `maestro verdict request --task <id>`
- Recovery: `maestro recover --task <id>`
- Convergence oracle: `maestro ralph review --task <id>`

## Agent skill lookup

1. `.maestro/skills/{agentType}/SKILL.md`
2. `skills/built-in/{agentType}/SKILL.md`

## Project conventions

Repo-level code conventions, build commands, and feature boundaries live in the
project root `AGENTS.md` and `CLAUDE.md`. This file holds only the harness
pointer surface.
