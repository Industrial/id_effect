# release-0-9 — execution overlay

Mission `pln-mvbdsaam-th3g19` · spec `.maestro/specs/releases/0.9.0.md` (approved 2026-10-16) ·
branch `release/0.9.0` (off `release/0.8.0`; PR #105 carries 0.8.0, so 0.9 commits stay off it).

## Wave 1

Wave 1 starts in two steps: 1a needs no new crate and no `SqlValue`; 1b waits for the crate
skeletons (T0) and for the owner-visible `SqlValue` design review.

### 1a (dispatched at kickoff)

| Agent | Leaf (task id) | Files (owned) |
|---|---|---|
| A0 governance | `r09-t0-adr-skeletons` `tsk-mvbdsp4f-xmojqz` | docs/adrs/0010-crate-tiers.md (Amendment 5), root Cargo.toml members, crates/id_effect_{auth,authz,test}/ skeletons, scripts/validate-crate-packaging.sh, .github/workflows/{publish,ci}.yml |
| A1 sql design | `r09-e3-1a-values-rows` `tsk-mvbdsp4f-wd3fai` — **design note only** | .maestro/specs/releases/0.9/e3-1a-sql-values-design.md (no source) |
| A2 routes | `r09-e4-1-route-descriptor` `tsk-mvbdsp4f-ex9rcm` | crates/id_effect_axum/src/route/**, id_effect_axum Cargo.toml + lib.rs (own lines) |
| A3 profiles | `r09-e5-4-profiles` `tsk-mvbdsp4f-p6l6nv` | crates/id_effect_config/src/profile*.rs (new) + lib.rs lines, config tests |

### 1b (after A0 merges and the design note is approved)

| Leaf (task id) | Depends on |
|---|---|
| `r09-e3-1a-values-rows` `tsk-mvbdsp4f-wd3fai` (implementation) | design note approved |
| `r09-e3-1a-from-row` `tsk-mvbdsp4f-e7yo8j`, `r09-e3-1a-tx-stream` `tsk-mvbdsp4f-spcif4` | values-rows |
| `r09-e3-1a-ripple-docs` `tsk-mvbdsp4f-ua6x6x` | values-rows; tell the session that owns examples/http-showcase first |
| `r09-e3-2-migrate-api` `tsk-mvbdsp4f-a1hau9`, `r09-e3-2-cli` `tsk-mvbdsp4f-2m3dmu`, `r09-e3-2-docs` `tsk-mvbdsp4f-0yq2ci` | values-rows manifest merged |
| `r09-e4-1-extractors-problem` `tsk-mvbdsp4f-cbg990`, `r09-e4-1-docs` `tsk-mvbdsp4f-y1xx0b` | route-descriptor |
| `r09-e5-2a-passwords-sessions` `tsk-mvbdsp4f-c6kk1f`, `r09-e5-2a-credentials-jwt` `tsk-mvbdsp4f-i8nl5p` | A0 (id_effect_auth skeleton) |
| `r09-e5-3-policies` `tsk-mvbdsp4f-bcinb5`, `r09-e5-3-guards-testing` `tsk-mvbdsp4f-5hrfbn` | A0 (id_effect_authz skeleton) |
| `r09-e5-4-vault` `tsk-mvbdsp4f-souo6g` | profiles |

Rules (as 0.8): agents never commit or stage; the main session reviews and commits each leaf
pathspec-limited. Integrator-only files: CHANGELOG.md, docs/MIGRATION-0.9.md, book SUMMARY.md,
docs/scorecard/. examples/http-showcase belongs to a concurrent session and is off-limits until the
ripple leaf is announced.

## Waves 2–3

Tasks are created with `task from-spec` when their wave starts (the mission can only be decomposed
once); see the spec's Waves table.

## Log

| Date | Event |
|---|---|
| 2026-10-16 | Spec approved (D1 (b), D2–D12 as recommended). Mission created, wave 1 decomposed (17 leaves). Wave 1a dispatched. |
