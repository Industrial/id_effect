Produce a **Maestro-native hierarchical implementation plan** of the highest possible quality before writing or changing any code. Planning is the primary deliverable; implementation comes only after the plan is materialized in Maestro, validated, and the user approves (unless they explicitly asked for plan + implementation in one pass).

**MCP-first rule:** Prefer **Maestro MCP** (`project-0-solana-yield-optimizer-maestro`) for all mission/task/evidence/verdict/handoff/contract operations. Fall back to `devenv shell -- maestro …` CLI only when no MCP tool exists (noted below). Read each tool's schema under `mcps/project-0-solana-yield-optimizer-maestro/tools/` before calling — the schema is strict; unknown fields fail.

---

## Skills (load before planning)

When invoked with `/skills`, review `.cursor/skills/` and load **all** of these for this command:

| Skill | Role in this command |
|-------|----------------------|
| `.cursor/skills/maestro/SKILL.md` | Repo Maestro read order, heavy-mode loop, parallel waves |
| `~/.claude/skills/maestro-design/SKILL.md` | Grill protocol → `.maestro/specs/<slug>.md` |
| `~/.claude/skills/maestro-mission/SKILL.md` | Heavy-mode mission decomposition |
| `~/.claude/skills/maestro-task/SKILL.md` | Task claim/verify/ship loop, `task split` |
| `~/.claude/skills/maestro-verify/SKILL.md` | Witness levels, verdict routing, plan-check |
| `~/.claude/skills/maestro-handoff/SKILL.md` | Inbox check, `to_agent`, pickup protocol |
| `.cursor/skills/engineering/scrutinize/SKILL.md` | Self-review the plan before presenting |
| `.cursor/skills/science/scientific-method/SKILL.md` | Trade-off / optimization decisions in the plan |

Also use MCP servers in parallel during reconnaissance:

| Server | Use during planning |
|--------|---------------------|
| **lean-ctx** | `ctx_read`, `ctx_search`, `ctx_tree`, `ctx_shell` — token-efficient repo reads |
| **roam-code** | `roam preflight`, `roam impact`, `roam context` — blast radius before leaves |
| **serena** | `find_symbol`, `find_referencing_symbols` — API surface verification |
| **context7** | Library/framework docs for unfamiliar deps (`resolve-library-id` + `query-docs`) |
| **searxng** | External research when Context7 has no match |

---

## Mindset

- **Think before you act.** Decompose until each leaf is one PR, one session, one verifiable outcome (ADR-0006: 1 task ↔ 1 PR).
- **Ground every claim in evidence.** Read specs, ADRs, code paths. Cite real paths — never invent structure.
- **Prefer decisions over options.** Lock choices at the root; open questions only when blocking, each with a default and delta-if-wrong.
- **Design for Maestro verification.** Every leaf must define acceptance criteria, witness level, and gates mappable to `maestro task verify` + `maestro verdict request`.
- **Parallelize aggressively.** Reconnaissance and independent wave tasks run as concurrent subagents (see Phase 1 and Phase 7).

---

## Phase 0 — Maestro bootstrap (mandatory, MCP)

Run these **in parallel** where independent:

```
maestro_setup_check                          → harness scaffold ok?
maestro_handoff_list { view: "summary" }    → open envelopes from prior agents
maestro_task_list { state: "draft" }         → unclaimed work
maestro_task_list { state: "blocked" }       → blockers to resolve first
maestro_task_list { state: "claimed" }       → in-flight ownership
```

CLI complements (no MCP equivalent):

```bash
devenv shell -- maestro status --json
devenv shell -- maestro doctor
devenv shell -- maestro intake --paths <touched-paths>
```

Read order after bootstrap:

1. `.maestro/MAESTRO.md` → `.maestro/tasks/NOW.md` → active spec/mission
2. `.maestro/policies/*.yaml` — risk, autopilot, sensitive-paths
3. Existing `.maestro/missions/*.execution.md` — do not contradict wave tables
4. `.cursor/plans/` — avoid duplicate plans

If `maestro_handoff_list` returns envelopes for your tool (`to_agent` filter), run pickup protocol per `maestro-handoff` before planning.

---

## Phase 1 — Parallel reconnaissance (mandatory subagents)

**Do not draft the plan in the parent agent until reconnaissance completes.**

Launch **multiple Task subagents in a single message** (Cursor parallel subagents — one Task call per independent track). Minimum tracks:

| Subagent | `subagent_type` | Deliverable |
|----------|-----------------|-------------|
| Codebase map | `explore` | Files/modules the change touches; existing patterns to follow |
| Maestro state | `explore` | Active missions, specs, execution overlays, blocked tasks |
| Blast radius | `explore` or roam | `roam preflight` / `roam impact` on 2–5 key symbols |
| Dependency docs | `generalPurpose` | Context7 lookup for unfamiliar libraries in the change |

Each subagent prompt must include: goal, paths to search, output format (bullet list + file paths), and **no implementation**.

Parent synthesizes into a **Reconnaissance digest** table:

| Finding | Source | Implication for plan |
|---------|--------|----------------------|

Do not skip reconnaissance. A plan without codebase grounding is invalid.

---

## Phase 2 — Scope, intake, and spec

### 2a. Lock scope

Restate in one paragraph: goal, in-scope, out-of-scope, assumptions. Run intake:

```bash
devenv shell -- maestro intake --paths <comma-separated-paths>
```

Route by lane:

| Lane | Next step |
|------|-----------|
| `tiny` | Single leaf plan; `maestro_task_from_spec` or inline spec |
| `normal` | Light spec → one task |
| `high-risk` | Heavy spec + threat-model evidence requirement |

### 2b. Author or load spec

- **No spec:** load `maestro-design` skill; grill to `.maestro/specs/<slug>.md`; validate:

  ```bash
  devenv shell -- maestro spec validate .maestro/specs/<slug>.md
  ```

- **Spec exists:** read it; confirm `mode`, `acceptance_criteria`, `non_goals`, `risk_class`.

### 2c. Preview policy (when task id known)

```
maestro_policy_check { taskId: "tsk-..." }
```

Record effective risk class and sensitive-path matches in the plan's decision log.

---

## Phase 3 — Hierarchical decomposition

Build a tree aligned with Maestro artifacts:

```
Epic (user goal)
├── Phase / milestone (coherent value slice)
│   ├── Work package (1–3 days)
│   │   └── Leaf task → future tsk-… (one PR)
```

**Rules:**

- **Leaves only** carry full detail (sections below). Parents summarize intent, dependencies, rollup AC.
- Each leaf is **MECE** under its parent.
- Order by dependency (topological). Mark **parallel siblings** explicitly — these become wave rows.
- Cap leaf size: one module boundary, one API surface, one migration, one testable behavior.
- Stable IDs: `leaf-env-settings`, `leaf-pipeline-wire`, etc. — match Maestro task slugs.
- Heavy mode (`mode: heavy` in spec): 3+ leaves → mission + execution overlay.
- Light mode: 1 leaf → single `maestro_task_from_spec`.

---

## Phase 4 — Materialize in Maestro (MCP)

### Heavy mode (multi-PR)

**Option A — from spec (preferred):**

```
maestro_mission_from_spec { spec_path: ".maestro/specs/<slug>.md" }
→ pln-...
maestro_mission_decompose {
  mission_id: "pln-...",
  tasks: [
    { title: "...", slug: "leaf-..." },
    ...
  ]
}
maestro_mission_show { mission_id: "pln-..." }
```

**Option B — from task batch file:**

```
maestro_mission_new { title: "...", mode: "from-file", from_file: "tasks.json" }
```

**Option C — template:**

```
maestro_mission_new { title: "...", mode: "template", template: "feature" }
```

### Light mode (single PR)

```
maestro_task_from_spec { spec_path: ".maestro/specs/<slug>.md" }
→ tsk-...
maestro_task_get { id: "tsk-..." }
```

### Execution overlay (heavy mode — mandatory)

Write `.maestro/missions/<slug>.execution.md` with a **wave table** (see `hyperliquid-market-data.execution.md`):

```markdown
# Execution overlay: <slug>

| Wave | Tasks (slug) | Parallel? | Blocked by |
|------|--------------|-----------|------------|
| 0    | leaf-a, leaf-b | yes     | —          |
| 1    | leaf-c       | no        | wave 0     |
```

Rules:

- **Never claim wave N+1 tasks until wave N tasks are `shipped`.**
- Parallel wave → one subagent per task, launched in **one parent message**.
- Sequential chain → `blocked_by` edges in decompose batch or `maestro_task_split` without `parallel`.

### Mission sidecar (optional, recommended)

Write `.maestro/missions/<slug>.md` — objective, phases, verification, risks (per `maestro-mission` skill).

---

## Phase 5 — Plan document + validation

Write `.cursor/plans/<slug>.plan.md`:

```yaml
---
name: <Human-readable title>
overview: <One sentence>
maestro:
  mission_id: pln-...   # or task_id: tsk-... for light mode
  spec_path: .maestro/specs/<slug>.md
  execution_overlay: .maestro/missions/<slug>.execution.md  # heavy only
todos:
  - id: leaf-...
    content: <imperative title>
    status: pending
isProject: false
---
```

Then full hierarchical body (see **Required content per leaf** below).

### Plan-check gate (CLI — no MCP tool)

After the plan file exists and a task is materialized:

```bash
devenv shell -- maestro plan check --task <tsk-id> --plan-file .cursor/plans/<slug>.plan.md
```

Fix `scope-widens`, `missing-proof`, `risk-class-too-low` before presenting.

Record result:

```
maestro_evidence_record { taskId: "tsk-...", note: "plan-check PASS: <summary>" }
```

---

## Phase 6 — Self-review before presenting

Load `scrutinize` skill on the draft plan:

1. **Intent** — simpler alternative?
2. **Trace** — each leaf's file paths exist?
3. **Verify** — AC falsifiable? Gates real?

Present plan to user. **Do not implement** until approved (unless user asked for both).

When invoked with `/quality`: sharpen AC, tighten scope, resolve ambiguities, re-run plan-check if task exists.

---

## Phase 7 — Parallel execution playbook (post-approval)

When user approves and work begins:

### Wave dispatch (maximum parallelization)

For each **parallel wave** in the execution overlay, launch **N Task subagents in one message** — one subagent per task in the wave. Each subagent prompt must include:

- `tsk-...` id and leaf AC from the plan
- `spec_path`, `worktree_path` (heavy mode)
- Gates to run before ship
- `tool: "cursor"` (or agent name) for handoff continuity

Parent agent **does not implement wave tasks itself** when subagents are available — orchestrate, merge, unblock.

### Per-subagent Maestro loop (MCP)

```
maestro_task_claim       { id, agent_id, tool: "<your-tool-name>" }
maestro_contract_show    { taskId }           # read scope before editing
... implement leaf ...
maestro_contract_amend   { taskId, addPaths, reason }   # if scope grew legitimately
maestro_evidence_record  { taskId, command, exitCode }  # after each gate
maestro_policy_check     { taskId }           # before verdict
maestro_verdict_request  { taskId }
maestro_verdict_show     { taskId }           # confirm PASS
maestro_task_ship        { id, pr_url? }
```

CLI steps without MCP:

```bash
devenv shell -- maestro task verify <tsk-id>
devenv shell -- maestro verdict request --task <tsk-id>
```

### Intra-task parallelism

When one leaf is still too large:

```
maestro_task_split {
  parent_id: "tsk-...",
  titles: ["slice A", "slice B", "slice C"],
  parallel: true,
  agent_id: "<claimant>"
}
```

Launch one subagent per child; ship children before parent.

### Handoffs (cross-session / cross-tool)

| Situation | MCP action |
|-----------|------------|
| Session start | `maestro_handoff_list { to_agent: "<tool>" }` |
| Read envelope | `maestro_handoff_show { id: "hnd-..." }` |
| Mark read | `maestro_handoff_pickup { id, picked_up_by: "<tool>" }` |
| Mid-stream handoff (verify/ship paths) | `maestro_handoff_emit { task_id, trigger_verb, to_agent, reason?, worktree_path?, spec_path? }` |
| Blocked | `maestro_task_block { id, reason, tool }` — auto-emits envelope |
| Abandon | `maestro_task_abandon { id, reason, cascade? }` |
| Cancel mission | `maestro_mission_cancel { mission_id, reason? }` |

Do **not** re-emit handoffs for `claim`/`block` — those verbs emit automatically.

### Evidence audit mid-wave

```
maestro_evidence_list { taskId, view: "summary" }
```

Promote recurring lint fixes:

```
maestro_principle_promote { correction_id: "evd-..." }
```

---

## Required content per leaf

Every **leaf** MUST include all subsections. No "TBD."

### 1. Context

- Why this leaf exists; what breaks if skipped
- Current state (verified paths)
- Target state (one paragraph)
- Dependencies (other leaf slugs / `blocked_by`)
- **Maestro:** intended `tsk-` slug, wave number, parallel group

### 2. Acceptance criteria

Given/When/Then or numbered **must** statements — objectively checkable. Include negative and edge cases. Map each AC to a test or command.

### 3. File & module structure

- **Create** / **Modify** / **Delete** with purpose
- Tree listing with real paths
- Public API additions explicit
- **Contract paths** for `maestro_contract_show` / amend if non-obvious

### 4. Diagrams

At least one diagram per **phase**. Control-flow leaves need sequence or state diagrams. Use real module/event names.

| Situation | Diagram type |
|-----------|--------------|
| Module boundaries | `flowchart TB` |
| Request lifecycle | `sequenceDiagram` |
| FSM / workflow | `stateDiagram-v2` |
| Data model | `erDiagram` |
| Rollout / waves | Wave table + dependency graph |

### 5. Quality gates

| Gate | Command | Pass | Witness level |
|------|---------|------|---------------|
| Unit | project-specific | 0 failures | agent-claimed-locally |
| Lint/format | `moon run :check` etc. | clean | agent-claimed-locally |
| Integration | named scenario | stated outcome | witnessed-by-ci if CI runs it |
| Maestro verify | `maestro task verify <tsk>` | exit 0 | witnessed-by-maestro |
| Verdict | `maestro_verdict_request` | PASS | witnessed-by-maestro |
| Repo gate | `verify-fast.sh` / definitively gate | exit 0 | witnessed-by-ci |

Record each gate via `maestro_evidence_record`. **Definition of done** = all gates + AC satisfied + `maestro_task_ship`.

### 6. Implementation notes

- Patterns to follow (link existing code)
- Error conventions
- Scope traps / anti-patterns
- Changelog if user-visible

### 7. Risks & rollback

- Severity, mitigation, feature-flag or revert path

---

## Phase 8 — Plan-level rollup

After all leaves:

1. **Executive summary** — 3–5 sentences
2. **Decision log** — locked choices + `maestro_policy_check` risk class
3. **Dependency graph** — mermaid with wave annotations
4. **Recommended order** — leaf slugs with one-line rationale
5. **Parallelism map** — which waves launch N subagents concurrently
6. **Total quality gate** — full-suite command(s)
7. **Out of scope / deferred**
8. **Maestro artifacts produced** — spec path, `pln-`/`tsk-` ids, execution overlay path

---

## Complete Maestro MCP tool reference

Use **every** tool where applicable — not just claim/ship.

| Tool | When in this workflow |
|------|----------------------|
| `maestro_setup_check` | Phase 0 bootstrap |
| `maestro_handoff_list` | Phase 0 inbox; Phase 7 session resume |
| `maestro_handoff_show` | Read specific envelope |
| `maestro_handoff_pickup` | After reading envelope |
| `maestro_handoff_emit` | Mid-stream handoff (verify/ship/abandon paths) |
| `maestro_mission_new` | Create mission (bare/from-file/template) |
| `maestro_mission_from_spec` | Heavy spec → approved mission |
| `maestro_mission_decompose` | Batch-create child tasks |
| `maestro_mission_show` | Inspect mission + children |
| `maestro_mission_cancel` | Abort mission + cascade abandon |
| `maestro_task_from_spec` | Light spec → draft task |
| `maestro_task_list` | Discovery by state/mission |
| `maestro_task_get` | Full task detail + children |
| `maestro_task_claim` | Start work (+ auto worktree heavy) |
| `maestro_task_split` | Parallel/sequential child slices |
| `maestro_task_block` | Block with reason |
| `maestro_task_abandon` | Drop task (+ cascade) |
| `maestro_task_ship` | ready → shipped |
| `maestro_contract_show` | Read scope before edit |
| `maestro_contract_amend` | Legitimate scope expansion |
| `maestro_policy_check` | Risk class + sensitive paths |
| `maestro_evidence_record` | After every gate |
| `maestro_evidence_list` | Audit trail mid-task |
| `maestro_verdict_request` | Pre-ship verdict |
| `maestro_verdict_show` | Confirm last verdict |
| `maestro_principle_promote` | Promote lint fix to principle |

**CLI-only** (no MCP — use `devenv shell -- maestro …`):

| Verb | When |
|------|------|
| `spec validate` | After authoring spec |
| `spec new` | Create spec stub |
| `intake --paths` | Pre-code risk lane |
| `plan check --task --plan-file` | Validate plan vs contract |
| `task verify` | Architecture lint gate |
| `status`, `doctor`, `recover` | Session health / recovery |
| `task observe` | Dev-time metrics/logs (non-gating) |

---

## Quality bar (self-check)

Reject your draft if any fail:

- [ ] Phase 0 MCP bootstrap ran; handoffs checked
- [ ] Reconnaissance used **parallel subagents**, not sequential parent reads
- [ ] Every leaf has AC, files, diagrams (where needed), gates — no exceptions
- [ ] Leaf slugs match Maestro task slugs; heavy mode has execution overlay + wave table
- [ ] All 26 Maestro MCP tools considered; each applicable tool invoked or explicitly N/A
- [ ] `maestro plan check` run when task exists
- [ ] Paths verified against repo (roam/serena/lean-ctx — not guessed)
- [ ] Parallel waves map to **single-message multi-Task** dispatch
- [ ] Dependencies acyclic; wave order executable
- [ ] Mid-level engineer could implement any leaf without clarifying questions

---

## Anti-patterns (do not)

- Planning in the parent while skipping parallel recon subagents
- Flat bullet lists without hierarchy, wave numbers, or Maestro ids
- Hand-writing heavy specs without `maestro-design` grill
- `maestro_task_from_spec` on a `mode: heavy` spec (orphan task — use mission path)
- Claiming multiple wave tasks on one agent when subagents are available
- Sequential Task launches when tasks are in the same parallel wave
- Skipping `maestro_contract_show` before editing scoped tasks
- Skipping `maestro_evidence_record` / `maestro_verdict_request` before ship
- Re-emitting handoffs for claim/block (already automatic)
- Starting implementation before plan-check PASS and user approval
- Using CLI when MCP tool exists for the same operation

---

## When the user attaches modifiers

| Modifier | Action |
|----------|--------|
| `/skills` | List all skills reviewed (table above) before continuing |
| `/quality` | Re-read request + this command; sharpen AC, scope, diagrams, gates; re-run plan-check |
| `/scientific-method` | For optimization/trade-off leaves: observations, ranked hypotheses, falsification-first experiments in the plan |
| `/scrutinize` | Outsider review of the plan before presenting |
