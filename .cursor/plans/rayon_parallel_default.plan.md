---
name: Rayon parallel-by-default
overview: Make bulk pure transforms and stream chunk ops parallel (Rayon) by default, with explicit serial escape hatches; keep Effect sequencing sequential unless using bounded effectful parallelism.
todos:
  - id: leaf-spec-rayon-default
    content: "Wave 0: Author spec + ADR for parallel-by-default policy"
    status: completed
  - id: leaf-parallelism-core
    content: "Wave 1: Introduce Parallelism type and dispatch helpers"
    status: completed
  - id: leaf-collections-default-par
    content: "Wave 2: Wire collections + vec/order to parallel default"
    status: completed
  - id: leaf-stream-default-par
    content: "Wave 3: Wire Stream map/filter to parallel default + effectful API clarity"
    status: completed
  - id: leaf-api-migration-serial
    content: "Wave 4: Add *_serial aliases; deprecate *_par; update call sites"
    status: completed
  - id: leaf-docs-skill-examples
    content: "Wave 5: Book, SKILL, examples, CHANGELOG migration notes"
    status: completed
  - id: leaf-verify-bench-ship
    content: "Wave 6: Proptest parity, benches, full workspace gates"
    status: completed
isProject: false
---

# Rayon parallel-by-default — hierarchical plan

## Skills reviewed

| Skill | Role |
|-------|------|
| `.cursor/skills/maestro/SKILL.md` | Mission waves, verify/ship loop |
| `.cursor/skills/id_effect/SKILL.md` | Library API conventions (3.0 DI) |
| `~/.claude/skills/maestro-design/SKILL.md` | Spec grill → `.maestro/specs/` |
| `~/.claude/skills/maestro-mission/SKILL.md` | Heavy-mode decomposition |
| `~/.claude/skills/maestro-verify/SKILL.md` | Witness levels, gates |

## Reconnaissance digest

| Finding | Source | Implication |
|---------|--------|-------------|
| Rayon is a hard dependency (`rayon = "1.12"`) | `crates/id_effect/Cargo.toml` | No new dep; optional `serial-only` feature deferred |
| Parallel APIs are additive `*_par` siblings | `stream.rs`, `hash_map.rs`, `functor.rs`, `order.rs`, RBT, trie | Inversion = default dispatch + serial escape |
| `Effect` kernel is sequential | `kernel/effect.rs` | `effect!` stays ordered; parallelism in collections + Stream bulk ops |
| `map_par` needs `Send + Sync`; serial `map` allows `FnMut` | `stream.rs:638–696` | Parallel default breaks some call sites → `*_serial` required |
| `map_par_n` is async bounded concurrency, not Rayon | `stream.rs:960+` | Keep separate; document as effectful parallel |
| Parity tests: `*_par` ≡ serial today | tests in stream/hash_map/order | Become default-vs-serial regression suite |
| No book `_par` docs yet | book grep | Teach parallel default from scratch |
| No Maestro spec for rayon default | `.maestro/specs/` | New spec before implementation |

## Executive summary

**Yes — parallel-by-default is feasible** for pure bulk ops (Vec map, HashMap map/filter, RBT scans, Stream chunk map/filter, sort). The **`Effect` monad and `effect!` remain serial**; only data-parallel layers flip default.

**Locked decision:** add `Parallelism` policy (`Auto` with threshold default, `ForceParallel`, `Serial`). Primary methods dispatch through it. Explicit `*_serial` for non-`Send` / captured-mut closures. Deprecate `*_par` as aliases to `ForceParallel`.

**Out of scope:** parallel `effect!` binds; WASM serial crate; changing `map_par_n` semantics.

## Wave table

| Wave | Task slug | Parallel? | Blocked by |
|------|-----------|-----------|------------|
| 0 | `leaf-spec-rayon-default` | no | — |
| 1 | `leaf-parallelism-core` | no | 0 |
| 2 | `leaf-collections-default-par` | yes | 1 |
| 3 | `leaf-stream-default-par` | yes | 1 |
| 4 | `leaf-api-migration-serial` | no | 2–3 |
| 5 | `leaf-docs-skill-examples` | yes | 4 |
| 6 | `leaf-verify-bench-ship` | no | 5 |

## Leaf summaries

### `leaf-spec-rayon-default`
- Create `.maestro/specs/id-effect-rayon-default.md`, ADR `docs/adrs/0006-parallel-by-default.md`, execution overlay.
- AC: `maestro spec validate` pass; documents Effect vs Stream boundary.

### `leaf-parallelism-core`
- New `crates/id_effect/src/parallelism.rs` with `Parallelism` enum + dispatch helpers.
- Default: `Auto { threshold: 1024 }`.

### `leaf-collections-default-par`
- Wire `vec::map`, `map_values`, `filter`, RBT/trie/order primary fns through policy.
- Add `*_serial`; deprecate `*_par`.

### `leaf-stream-default-par`
- `Stream::map` / `filter` → policy default; add `map_serial`, `filter_serial`, `map_with(policy, f)`.
- Leave `map_par_n` unchanged.

### `leaf-api-migration-serial`
- Repo-wide `_par` call site cleanup; `#[deprecated]` shims.

### `leaf-docs-skill-examples`
- SKILL + book chapter + example `071_stream_map_serial.rs`.

### `leaf-verify-bench-ship`
- Full workspace test/clippy/mdbook; mission verify + ship.

## Decision log

| Decision | Choice |
|----------|--------|
| Default policy | `Auto { threshold: 1024 }` (not blind ForceParallel) |
| Serial escape | `*_serial` methods |
| `effect!` | Always serial |
| `map_par_n` | Unchanged |

## Open question

Always-parallel vs Auto default? **Recommend Auto** — say if you prefer ForceParallel for simpler docs.

## Next steps (after approval)

1. Author spec via maestro-design grill
2. `maestro_mission_from_spec` + decompose 7 leaves
3. `maestro plan check`
4. Implement wave-by-wave

**Plan file:** `.cursor/plans/rayon_parallel_default.plan.md`
