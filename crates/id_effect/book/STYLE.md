# Typed Effects in Rust — Editorial Style Guide

This document defines how the book is written. Every chapter should read like a **tutorial for a competent Rust async developer who is new to id_effect**, not like internal planning notes, crate READMEs, or ADR archives.

## Reader

**Assume:**

- Comfortable with Rust: ownership, traits, `Result`, `async`/`await`, and `Future`.
- Building or maintaining real async services (CLI tools, HTTP APIs, background workers).
- No prior exposure to id_effect, Effect.ts, or category theory.

**Do not assume:**

- Knowledge of workspace layout, mission names, parity phases, or ADRs.
- That the reader works on the id_effect repository.

## Voice and tone

| Do | Don't |
|----|-------|
| Explain **why** before **what** | Open with crate names or module tables |
| Use second person ("you") sparingly and naturally | Write changelogs ("we added", "Phase G delivers") |
| Name problems readers recognize (flaky retries, DI sprawl) | Reference `docs/effect-ts-parity/`, missions, or ADRs in body text |
| State scope and limits honestly | Oversell experimental crates as production defaults |
| Prefer plain language | Jargon without definition |

**Banned in chapter prose** (fine in contributor-only docs):

- `Mission: …`, `phase-*`, `ADR-*`, `tsk-*`, `pln-*`
- Links to `docs/platform/ROADMAP.md` or parity phase folders as primary explanation
- "This chapter documents…" — teach instead

**Allowed once per book** where relevant: a short "For contributors" box in an appendix pointing to internal design docs.

## Chapter anatomy

Every chapter file uses this structure. Omit sections only when genuinely empty (e.g. no exercises in a one-page bridge).

```markdown
# Title — outcome-oriented, not crate name alone

One paragraph: the **problem** this chapter solves and where it sits in the learning path.

## What you'll learn

- Bullet list of 3–5 concrete outcomes (skills, not API names).

## Prerequisites

- Links to prior chapters the reader should have read.

## … teaching sections …

Each section: concept → minimal example → slightly richer example → pitfall or "when not to use".

## Putting it together

Runnable or near-runnable program tying the chapter together (may continue a running example).

## Summary

Three to five sentences: what changed in the reader's mental model.

## Next steps

Links to the next chapter(s) in reading order.
```

### Section rules

1. **Lead with motivation**, not API tables. Tables belong after the reader knows why the table exists.
2. **One new idea per section.** If a section needs three H3s, consider splitting the file.
3. **Examples must teach a story.** Prefer a single evolving domain (e.g. a small `orders` service) over disconnected `foo`/`bar` snippets.
4. **Show the edge.** Every integration chapter ends with how to run, test, and swap implementations.
5. **Link to `docs.rs` for exhaustive API**; the book teaches patterns, not every method.

## Code examples

- **Compile** unless explicitly marked `ignore` or `no_run` with a comment why.
- **Complete enough to copy**: imports, `main` or test harness when the snippet is meant to run.
- **Prefer workspace crates** as documented in `Cargo.toml`; pin narrative to stable public paths.
- **Name types for clarity** over brevity (`OrderRepository`, not `Repo`).
- After non-obvious blocks, add one sentence on what just happened.

## Integration and platform chapters

Chapters about workspace crates (`id_effect_axum`, `id_effect_host`, …) follow the **integration template**:

1. **When you need this** — decision criteria vs alternatives (raw Tokio, raw Axum, other stacks).
2. **Mental model** — how the crate sits on `Effect<A, E, R>` (one diagram or paragraph).
3. **Minimal wiring** — smallest program that works.
4. **Production concerns** — config, errors at the boundary, testing doubles.
5. **Experimental / limits** — semver, single-process vs distributed, what to use instead at scale.

Do **not** duplicate content across Part II (early wiring) and Part VI (platform depth). Part II teaches the **pattern once**; Part VI goes **deeper on operations** without re-listing every type.

## Book structure conventions

| Part | Role |
|------|------|
| I | Motivation + core `Effect` + `effect!` |
| II | `R`, capabilities, providers, services — **one** full DI example |
| III | Production mechanics: errors, fibers, resources, scheduling, CLI |
| IV | Advanced topics on demand: STM, streams, schema, testing |
| V | Optional functional patterns (optics, FSM, parsers, events) |
| VI | Application platform: observability, data, host, messaging, workflow |
| VII | Full-stack UI |
| Appendices | Reference, migration, glossary, contributor tooling |

**Reading order** in `SUMMARY.md` must match I → VII, then appendices.

**Chapter IDs** (`chNN-`) are stable anchors; prefer new suffix files (`ch07-13-…`) over duplicate `ch07-12-*` names.

## Cross-references

- Link to **other book chapters** for concepts.
- Link to **docs.rs** for API detail.
- Link to **repository examples** with `cargo run -p … --example …` when a full sample exists.
- Avoid linking to `docs/` tree in chapter body; fold essential context into prose.

## Review checklist (per chapter)

- [ ] Opens with a reader problem, not a crate name
- [ ] "What you'll learn" present
- [ ] No mission/phase/ADR references in prose
- [ ] At least one runnable or `no_run`-justified example
- [ ] Prerequisites and next steps linked
- [ ] Summary restates mental model, not API list
- [ ] `mdbook build` succeeds

## Pilot and rollout

1. Apply this guide to **Part I** (light edit — already closest to target).
2. Normalize **Part VI** (furthest from target).
3. Deduplicate **Part II ch07** integration sections vs Part VI.
4. Mechanical pass on Parts III–V, VII.
5. Appendices last (reference tone is acceptable).
