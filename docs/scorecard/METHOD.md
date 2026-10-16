# Platform Scorecard — Method (v2.1)

Since method 2.0 (2026-10-16) every score is a **checklist score** (§7): a row's score follows
mechanically from the level of a framework and the table-stakes checks it meets. §2–§3 describe the
bands and caps the checklist score is built on; the method 1.x judgment scores are kept as history
(§5.2, `rows[].v1`, `scripts/scorecard.sh --method 1`).

This scorecard measures how well id_effect serves a serious corporate team, compared with ten
reference frameworks. Every release is re-scored against it. The plan of plans
(`../../.maestro/specs/roadmap/PLAN-OF-PLANS.md`) sets a target per row per release, and each
release proves its progress by re-running this method.

- **Data:** `scorecard.json` (single source of truth: rows, competitor scores, id_effect scores per
  version, evidence, tracks, targets, v2 checklists).
- **Rendered view:** the published HTML matrix, `docs/scorecard/index.html`, generated from
  `scorecard.json` by `scripts/scorecard.sh --html` (CI checks it is up to date).
- **Computation:** this document, §5. It must be reproducible with nothing but `scorecard.json`.
  `scripts/scorecard.sh` implements it (bash + jq, same conventions as
  `scripts/bench-compare.sh`); `scripts/scorecard.sh --self-test` checks §5.2.

## 1. What is scored

| Term | Meaning |
|---|---|
| Framework | One of 11 columns. Scope per column is fixed in `scorecard.json → frameworks[].scope`. |
| Row (capability) | One of 82 capabilities, each with a stable `id` (e.g. `sec.authn`) and a one-sentence `measures` definition. Row ids never change. Rows are only added, never removed, and an addition bumps the method version. |
| Group | One of 13 groups. Every row belongs to exactly one. |
| Score | An integer in 1–100: how well the framework covers the row **out of the box**, judged against the best enterprise practice of the day. |

"Out of the box" means first-party, or a third-party library that the framework's own official docs
recommend. A check met only through a third-party library the docs do not recommend counts at the
`labs` level (§7.4, alignment rule).

## 2. Score anchors (all frameworks)

| Band | Meaning |
|---|---|
| 90–100 | Best in class. First-party, mature, documented, and what enterprises standardise on. |
| 70–89 | Strong. Minor gaps, or a widely adopted third-party standard. |
| 40–69 | Partial. Needs DIY wiring, is immature, or covers only the common path. |
| 10–39 | Primitives only, labs/experimental quality, or a stub. |
| 1–9 | Absent. |

## 3. Extra rules for the id_effect column

id_effect is scored from **this repository at a release tag**. These rules make a re-test give the
same answer whoever runs it.

### 3.1 Evidence
- Every id_effect score has an `evidence` string that names files, symbols, tests or bench results.
- A score can only change between versions if the evidence changes: new or removed code, tests, docs
  or measurements. Changelog claims alone do not count. The symbol must be found in the tree.

### 3.2 Hard caps (applied after the anchor judgment)

| Condition (at the scored tag) | Max score |
|---|---|
| Capability absent from published crates | 9 |
| Exists only in `labs/`, `examples/`, or an unpublished crate | 39 |
| Published, but no test exercises it | 49 |
| Published and tested, but no book chapter or rustdoc guide covers it | 69 |
| Score ≥ 80 requires all of: published, book chapter, runnable example, integration test | — |
| Score ≥ 90 also requires: covers the leader's table stakes for the row **and** is used by a production consumer or by a reference app tested in CI | — |

If the caps and the judgment disagree, the lower number wins.

### 3.3 Non-goals
A row marked `stance: non-goal` or `partial-non-goal` is **still scored**. The scorecard never
hides gaps. The stance only explains why a low score is accepted.

## 4. Competitor columns
- These are expert assessments, frozen as a **snapshot** with the date in
  `scorecard.json → competitor_snapshot`. The current snapshot is 2026-10-14.
- Snapshots are refreshed at most once a year, and always before 1.0. A refresh is stored as a new
  snapshot. Reports then show id_effect's index under **both** the old and the new snapshot, so that
  "we moved" and "they moved" can be told apart.
- Competitor scores are never changed in between snapshots, except to fix a factual error. Such a
  fix is logged in §8 with a reason.

## 5. Computation

Let `s(r, f)` be the score of row `r` for framework `f`, and let `G` be the set of groups, each with
a set of rows `R_g`.

1. **Group score** `GS(g, f) = mean over r in R_g of s(r, f)`.
2. **Unweighted index** `U(f) = mean over all 82 rows of s(r, f)`. Rows count equally, so large
   groups weigh more.
3. **Enterprise index** (headline) `E(f) = Σ_g w_ent(g)·GS(g,f) / Σ_g w_ent(g)`, using the
   `w_enterprise` weights in `scorecard.json → groups`. Groups count equally regardless of how many
   rows they have; the weight sets their importance to a corporate buyer.
4. **Runtime index** `RT(f)`: the same formula with `w_runtime`. It answers "how good is the
   engine?".
5. **Rank** of a framework under an index: its 1-based position when sorted descending. Ties take
   the better rank.
6. **Row KPIs** for id_effect:
   - `best_in_class` = number of rows with s ≥ 80
   - `red` = number of rows with s < 40
   - `leader` = number of rows where id_effect ≥ every competitor
   - `gap(r)` = best competitor score − id_effect score
   - `median_gap` = the lower median of gap over all rows (element ⌊n/2⌋ of the sorted list)
7. **Rounding:** compute in floating point and report to one decimal. Never round intermediate
   values.

### 5.1 Projected scores (plan tracking)
Each row has sparse `targets` of the form `{version: score}`. The **planned** id_effect score of a
row at version `v` is:

```
planned(r, v) = max( latest measured score of r at or before v,
                     max target score of r with target version ≤ v )
```

Applying §5 to planned scores gives the **target indices** for a release. At release exit, the
measured indices are compared with the target indices (see PLAN-OF-PLANS §6).

### 5.2 Reference results (method 2.1, competitor snapshot 2026-10-14)

| Version | U | E (rank) | RT (rank) | ≥80 | <40 | leader | median gap |
|---|---|---|---|---|---|---|---|
| 0.6.0 measured | 38.5 | 37.3 (#10) | 46.0 (#6) | 4 | 47 | 2 | 62 |
| 0.7.0 measured | 45.3 | 43.2 (#9) | 56.1 (#6) | 13 | 39 | 4 | 50 |
| 0.8.0 target | 54.0 | 53.0 (#6) | 63.3 (#3) | 23 | 31 | 7 | 37 |
| 0.9.0 target | 63.3 | 63.1 (#3) | 68.1 (#2) | 33 | 21 | 8 | 25 |
| 0.10.0 target | 70.1 | 70.3 (#3) | 74.6 (#1) | 40 | 11 | 9 | 12 |
| 0.11.0 target | 73.8 | 73.3 (#2) | 78.9 (#1) | 48 | 8 | 15 | 12 |
| 0.12.0 target | 77.6 | 77.4 (#1) | 81.0 (#1) | 49 | 3 | 16 | 12 |
| 1.0.0 target | 82.0 | 82.1 (#1) | 84.5 (#1) | 59 | 1 | 20 | 12 |

Competitor indices in the same snapshot (after the §7.4 alignment):

| Framework | U | E | RT |
|---|---|---|---|
| .NET | 75.1 | 77.3 | 71.1 |
| Spring | 70.1 | 73.0 | 63.8 |
| Phoenix | 60.3 | 59.6 | 59.5 |
| Effect-TS | 56.8 | 56.0 | 60.4 |
| ZIO | 54.2 | 51.8 | 56.6 |
| Laravel | 52.2 | 53.3 | 42.3 |
| Rails | 51.3 | 51.5 | 40.6 |
| Django | 45.5 | 45.6 | 36.7 |
| Next.js | 38.9 | 41.0 | 33.4 |
| OTP | 37.3 | 36.6 | 43.2 |

History: the same table by method 1.1 (`scripts/scorecard.sh --method 1`; the v1 judgment scores
with the §3.2 caps applied strictly, and the v1 targets). The competitor indices under 1.1 are those
of method 1.0 (.NET E 85.4, Spring 81.1, Phoenix 77.6, Laravel 71.3).

| Version | U | E (rank) | RT (rank) | ≥80 | <40 | leader | median gap |
|---|---|---|---|---|---|---|---|
| 0.6.0 measured | 44.2 | 42.6 (#11) | 53.3 (#9) | 9 | 36 | 2 | 47 |
| 0.7.0 measured | 47.7 | 45.6 (#11) | 57.8 (#7) | 16 | 34 | 3 | 40 |
| 0.8.0 target | 55.0 | 53.7 (#11) | 63.7 (#7) | 25 | 26 | 4 | 28 |
| 0.9.0 target | 62.5 | 62.2 (#9) | 67.6 (#6) | 35 | 20 | 4 | 20 |
| 0.10.0 target | 69.3 | 69.2 (#5) | 73.8 (#4) | 41 | 11 | 6 | 15 |
| 0.11.0 target | 73.0 | 72.2 (#4) | 78.4 (#2) | 50 | 8 | 8 | 12 |
| 0.12.0 target | 76.6 | 76.4 (#4) | 80.7 (#1) | 51 | 3 | 8 | 12 |
| 1.0.0 target | 80.3 | 80.3 (#3) | 83.3 (#1) | 59 | 1 | 10 | 11 |

Under method 1.0 (before the caps) the 0.7.0 row read U 49.1, E 46.8 (#11), RT 59.1 (#7).

Any implementation of §5 **must** reproduce these tables exactly from `scorecard.json`. They are the
tool's self-test fixture.

## 6. Re-test protocol (each release)

1. Check out the release tag in a clean worktree.
2. A **fresh, blind assessor** re-scores the id_effect column. This is a new agent session or a
   person who has not seen the targets, given only this document, the previous `evidence` strings
   and the repo. Per row they record the level and the met checks (`rows[].v2.level`/`met`) with
   evidence per met check (`rows[].v2.evidence`), and update the row's `evidence` string.
3. Compute each row's checklist score (§7.2); the level applies the §3.2 caps.
4. Adjudicate: any row whose blind score differs from the release owner's expectation by more than
   10 points is discussed, and the decision is recorded in the release report.
5. Write the checklist scores under the new version key in `scorecard.json → rows[].id_effect`
   (`--validate` fails if one differs from its checklist score), then recompute §5.
6. Publish `docs/scorecard/results/<version>.json` and `REPORT-<version>.md`. The report has: the
   indices table, the deltas against the previous version and against the targets, the rows that
   changed with their evidence, any regressions, and any misses with their carry-over.

## 7. Method v2 (checklists)

v1 is anchored judgment plus hard caps. v2 makes the row score mechanical.

### 7.1 Data
- **Checks.** Each row has 4–8 **table-stakes checks**, `rows[].checks: [{id, text}]`, phrased so a
  yes/no answer is objective. An example for `sec.authn`: argon2 password hashing, server-side
  sessions with a persistent store, JWT verify/issue, OIDC with PKCE, MFA/TOTP, passkeys. Check ids
  are `<row id>#<n>`, unique within the row, and never reused.
- **Level.** `rows[].v2.level` gives every framework one level per row: `absent`, `labs`,
  `published`, `tested`, `documented` or `production`. id_effect has one level per version it has a
  score for. Competitor levels come from the frozen snapshot (§4).
- **Met.** `rows[].v2.met` lists the check ids that each framework meets, in the same shape as the
  level. id_effect checks come from the repository at the release tag, and `rows[].v2.evidence`
  maps each met check id to a path, symbol or test. Competitor checks are expert judgement from the
  snapshot. Labs or third-party answers can meet a check; their level caps the score.
- **Notes.** `rows[].v2.notes` is optional. It maps a framework id to a one-line reason why its v2
  score is far from its v1 score.

`scripts/scorecard.sh --validate` enforces this shape: 4–8 checks, known check ids only, an entry
for every framework, and id_effect versions that match its scores.

### 7.2 Score
For a row with `n` checks, a framework at level `L` that meets `m` of them scores:

```
s = clamp(1, cap(L), round(100 × m / n))
```

| Level | Meaning (at the scored tag) | cap(L) |
|---|---|---|
| `absent` | Capability absent from published crates | 9 |
| `labs` | Only in `labs/`, `examples/` or an unpublished crate | 39 |
| `published` | Published, but no test exercises it | 49 |
| `tested` | Published and tested, with no book chapter or rustdoc guide | 69 |
| `documented` | Published, tested, book chapter, runnable example and integration test | 89 |
| `production` | All of the above, plus a production consumer or a reference app tested in CI | 100 |

The caps are those of §3.2. `round` is half away from zero, computed in integers as
`floor((200 × m + n) / (2 × n))`, so 12.5 becomes 13. `clamp` keeps the score in 1–100. Indices,
ranks and KPIs then follow §5 unchanged.

`scripts/scorecard.sh` scores a file by its own method (the major of `method_version`); `--method 1`
reads the method 1.1 history of a method 2 file (`--version`, `--targets`, `--json`, `--diff`).
`scripts/scorecard.sh --method-compare` writes `REPORT-method-v2.md`, and CI checks that it is up to
date. The report shows, for each measured version:
- v1 (1.1) and v2 indices for all 11 frameworks;
- every id_effect row, v1 beside v2;
- every id_effect score and v1-leader score that moves by more than 15, with its note;
- every **v1 cap violation**: an id_effect v1 score above the cap of its v2 level (empty from 1.1 on).

### 7.3 Status
v2 is **primary since method 2.0** (owner decision L13, PLAN-OF-PLANS §7.0, re-based 2026-10-16).

### 7.4 Method 2.0: storage, alignment and re-based targets
- **Storage.** `rows[].competitors` and `rows[].id_effect` hold the checklist scores of §7.2.
  `--validate` fails when a stored score differs from the score its level and met list give, so the
  numbers cannot drift from the checks. `rows[].v1 {competitors, id_effect, targets}` keeps method
  1.1 for history; it is never edited again.
- **Method 1.1.** The v1 judgment scores, with every id_effect score above the cap of its v2 level
  lowered to that cap (25 scores on 17 rows), the targets unchanged.
- **Alignment rule (competitors).** For each met check of a competitor, its source is classified as
  first-party (inside `frameworks[].scope`, or the language runtime), `recommended` (a third-party
  library the framework's own official docs recommend or document as the way to do it), `other`
  (a third-party library they do not) or `not_met`. `not_met` checks are removed. A row with an
  `other` check scores the higher of: without the `other` checks at its level, or with them at the
  `labs` level (cap 39). The one-time pass is recorded check by check in `alignment-2.0.json`
  (194 cells reviewed, 60 scores changed); later snapshots apply the same rule directly.
- **Re-based targets.** A v1 target `t` on a row with `n` checks becomes the nearest achievable
  checklist score `round(100 × k / n)` (ties up), never below 40 or 80 when `t` was at or above that
  line (the red and best-in-class KPIs keep their meaning), and never below an earlier target of the
  same row. A target therefore reads "meet `k` of `n` checks"; `alignment-2.0.json →
  targets_rebased` lists each one.
- **Process rows (2.1).** A row marked `"process": true` scores a project policy, not a library
  capability: `sec.supply`, `test.qa`, `eco.stability`, `eco.backing`, `eco.community`,
  `eco.production`, `eco.license`. On these rows every framework's level is `production` when it
  meets at least one check and `absent` when it meets none, so the score is the share of checks met;
  a check counts as met only when it is a committed artifact (a file, a CI job or a published
  policy). `--validate` enforces the levels.

## 8. Change log

| Date | Method | Change |
|---|---|---|
| 2026-10-14 | 1.0 | Initial 82 rows × 11 frameworks; 0.6.0 scored. |
| 2026-10-15 | 1.0 | 0.7.0 re-scored (blind researcher passes on core and platform); `targets`, `track`, `stance` added; weights defined. Live-reload agent proposal 12 kept at 15, migrations proposal 6 kept at 3, and track record kept at 8, because under §3.1 the evidence had not changed (example-only or unmeasured). Compile-loop kept at 38 until measured. |
| 2026-10-15 | 1.0 | v2 checklists added (reported alongside). |
| 2026-10-16 | 1.1 | v1 caps applied strictly: 25 id_effect scores on 17 rows lowered to the cap of their v2 level (owner decision L13). |
| 2026-10-16 | 2.0 | v2 primary (L13). Competitor levels aligned by one rule (§7.4, `alignment-2.0.json`: 60 competitor scores changed); stored scores are checklist scores, validated; targets re-based (§7.4); method 1.1 kept in `rows[].v1`. ADR notes added at 0.7.0 for `test.unit` and `dx.diagnostics`, whose checklist scores drop from 0.6.0 (removed helpers and `id-effect-diagnose`). |
| 2026-10-16 | 2.1 | Process-row level rule (§7.4; owner decision D2 at 0.8 kickoff): 7 rows marked `process`; id_effect `eco.stability` 49 → 50 at 0.6.0 and 0.7.0 (no longer capped at `published`). `data.jsonschema` 0.8.0 target 75 → 50 (owner decision D1: OpenAPI from routes stays in 0.9). |
