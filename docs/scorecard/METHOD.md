# Platform Scorecard — Method (v2.2)

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
| Row (capability) | One of 82 capabilities, each with a stable `id` (e.g. `sec.authn`) and a one-sentence `measures` definition. Row ids never change. Rows are only added, never removed, and an addition bumps the method version. A non-goal row (§3.3) stays in the file but leaves the score, so 81 rows are scored since 2.2. |
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

### 3.3 Non-goals (method 2.2)
A row's `stance` (`non-goal`, `partial-non-goal`, …) only explains a score; it never changes one.
What leaves the score is the **non-goal status**, `non_goal: {"decision": "<id>"}` on a row or a
check. It applies to every framework alike:

- It is set **only by a locked decision** (PLAN-OF-PLANS §3.1), whose id the marker names (`L15`).
  `--validate` rejects a non-goal without one.
- **A check id_effect already meets is never a non-goal.** A non-goal is a check the platform has
  decided it will not meet; a met check stays scored. `--validate` fails if id_effect meets a
  non-goal check at any stored version.
- A **non-goal row** gets weight 0: it leaves its group mean, U, E, RT and the row KPIs (§5) for
  every framework, and it has no targets. It stays in `scorecard.json` (rows are never removed) and
  on the page, with its scores and stance `non-goal`.
- A **non-goal check** leaves its row's checklist: a framework meeting `m` of the row's other
  `n − k` checks scores `clamp(1, cap(L), round(100 · m / (n − k)))` (§7.2). Met lists may still name
  it for the record; it does not count.
- A check nobody can meet for a technical reason (`conc.safety#8` per-task heaps and
  `obs.tracing#8` zero-code agents in Rust) is not a decision, so it stays scored as a gap.

Non-goals since 2.2: row `web.graphql` (L15); checks `fault.reload#2`, `#6`, `#7` and
`dx.compile#3` (L8), `web.ui#3`, `#4` (L7), `data.backends#4`, `#5` (L6). `dx.repl#3` "live/hot
reload in dev mode" is not one: id_effect meets it (`id-effect dev`, the L8 substitute).

## 4. Competitor columns
- These are expert assessments, frozen as a **snapshot** with the date in
  `scorecard.json → competitor_snapshot`. The current snapshot is 2026-10-14.
- Snapshots are refreshed at most once a year, and always before 1.0. A refresh is stored as a new
  snapshot. Reports then show id_effect's index under **both** the old and the new snapshot, so that
  "we moved" and "they moved" can be told apart.
- Competitor scores are never changed in between snapshots, except to fix a factual error or to
  apply a scope change set by a locked decision (the 2.2 .NET scope, L15). Either is logged in §8
  with a reason, and the snapshot date stays.

## 5. Computation

Let `s(r, f)` be the score of row `r` for framework `f`, and let `G` be the set of groups, each with
a set of rows `R_g`. Every row below means a scored row: non-goal rows (§3.3) are left out of each
step, including the KPIs.

1. **Group score** `GS(g, f) = mean over r in R_g of s(r, f)`.
2. **Unweighted index** `U(f) = mean over all scored rows (81 since 2.2) of s(r, f)`. Rows count
   equally, so large groups weigh more.
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

### 5.2 Reference results (method 2.2, competitor snapshot 2026-10-14)

| Version | U | E (rank) | RT (rank) | ≥80 | <40 | leader | median gap |
|---|---|---|---|---|---|---|---|
| 0.6.0 measured | 39.2 | 37.8 (#10) | 46.5 (#6) | 4 | 45 | 2 | 61 |
| 0.7.0 measured | 46.1 | 43.7 (#9) | 56.5 (#6) | 13 | 37 | 4 | 38 |
| 0.8.0 measured | 57.0 | 55.6 (#5) | 64.9 (#2) | 28 | 29 | 7 | 25 |
| 0.9.0 measured | 68.5 | 68.1 (#3) | 71.4 (#2) | 39 | 19 | 20 | 12 |
| 0.10.0 measured | 76.6 | 76.6 (#2) | 79.5 (#1) | 49 | 11 | 26 | 12 |
| 0.11.0 measured | 79.4 | 79.2 (#1) | 83.9 (#1) | 56 | 9 | 31 | 12 |
| 0.12.0 measured | 82.3 | 81.5 (#1) | 85.9 (#1) | 59 | 7 | 37 | 12 |
| 0.13.0 measured | 86.7 | 86.8 (#1) | 88.7 (#1) | 64 | 3 | 40 | 11 |
| 1.0.0 target | 88.8 | 89.1 (#1) | 90.2 (#1) | 67 | 1 | 44 | 0 |

Competitor indices in the same snapshot (after the §7.4 alignment and the 2.2 .NET scope):

| Framework | U | E | RT |
|---|---|---|---|
| .NET | 76.7 | 78.8 | 72.8 |
| Spring | 70.6 | 73.6 | 64.5 |
| Phoenix | 60.6 | 59.9 | 59.7 |
| Effect-TS | 57.8 | 56.6 | 60.8 |
| ZIO | 53.9 | 51.5 | 56.6 |
| Laravel | 52.8 | 54.0 | 43.0 |
| Rails | 52.0 | 52.2 | 41.3 |
| Django | 46.4 | 46.4 | 37.4 |
| Next.js | 39.5 | 41.5 | 33.9 |
| OTP | 37.6 | 36.6 | 43.0 |

Of the .NET move (E 77.3 → 78.8), step 1 of the 2.2 re-base (non-goals) gives 77.9 and step 2
(Orleans and the Durable Task Framework first-party, §7.4) the other 0.9: `fault.reload` 80 → 100,
`msg.jobs` 39 → 75, `msg.cron` 39 → 50 (`alignment-2.2.json`).

History: the same tables by method 2.1, frozen at the 2.2 re-base (2026-10-18). They cannot be
recomputed from `scorecard.json` any more, because 2.2 changed .NET cells and moved targets
(web.rpc and web.realtime 0.11.0 → 0.12.0, the 0.11.0 bonus rows, web.graphql's targets removed).

| Version | U | E (rank) | RT (rank) | ≥80 | <40 | leader | median gap |
|---|---|---|---|---|---|---|---|
| 0.6.0 measured | 38.5 | 37.3 (#10) | 46.0 (#6) | 4 | 47 | 2 | 62 |
| 0.7.0 measured | 45.3 | 43.2 (#9) | 56.1 (#6) | 13 | 39 | 4 | 50 |
| 0.8.0 measured | 56.0 | 55.2 (#5) | 64.4 (#2) | 28 | 31 | 7 | 25 |
| 0.9.0 measured | 67.2 | 67.2 (#3) | 70.8 (#2) | 38 | 21 | 19 | 12 |
| 0.10.0 measured | 74.9 | 75.5 (#2) | 78.4 (#1) | 47 | 12 | 24 | 12 |
| 0.11.0 target | 78.9 | 78.8 (#1) | 82.9 (#1) | 55 | 8 | 30 | 12 |
| 0.12.0 target | 82.3 | 82.5 (#1) | 84.8 (#1) | 56 | 3 | 30 | 12 |
| 1.0.0 target | 84.9 | 85.0 (#1) | 87.1 (#1) | 61 | 1 | 34 | 12 |

Competitor indices under method 2.1 (after the §7.4 alignment, .NET scope before Orleans):

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

Older history: the same table by method 1.1 (`scripts/scorecard.sh --method 1`; the v1 judgment
scores with the §3.2 caps applied strictly, and the v1 targets; every row counts, including the
non-goal row). The competitor indices under 1.1 are those
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

Under method 1.0 (before the caps) the 0.7.0 row read U 49.1, E 46.8 (#11), RT 59.1 (#7). The
method 1.1 history stops at 0.7.0; 0.8.0 onward is scored by method 2.x only. Measuring a version
raises the planned scores of later targets (§5.1 takes the higher of the measured base and the
target), which is why the 0.9.0+ rows above moved when 0.8.0 was measured.

Any implementation of §5 **must** reproduce the method 2.2 tables and the method 1.1 history exactly
from `scorecard.json`. They are the tool's self-test fixture; the frozen 2.1 tables are not.

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

Since 2.2, `n` and `m` leave out the row's non-goal checks (§3.3): with `k` of them, the row scores
over `n − k` checks.

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
- **Method 2.2 (2026-10-18, PLAN-OF-PLANS §7.0b, L15).** Two changes, recorded cell by cell in
  `alignment-2.2.json`. (1) Non-goal rows and checks leave the score for every framework (§3.3);
  stored scores of the rows with non-goal checks are their checklist scores over the other checks,
  and their targets are re-based the §7.4 way ("meet `k` of `n`" becomes "meet `k` of `n − k'`",
  since id_effect meets none of the non-goal checks). (2) The .NET scope adds **Orleans** and the
  **Durable Task Framework** as first-party Microsoft. The 33 Orleans/Durable Task cells that
  `alignment-2.0.json` classified `recommended` become first-party (no score change, they already
  counted), and every fault, dist, conc and msg row was re-checked for checks Orleans or the Durable
  Task Framework meet that the snapshot did not credit, by the alignment rule above, against the
  public Microsoft docs (URLs per cell). Ten checks become met first-party: `fault.reload#4`
  (Orleans rolling upgrades) and `msg.jobs#3` were not credited before; `msg.jobs#1 #2 #5 #6 #7`
  and `msg.cron#4 #6 #7` were met only by Hangfire or Quartz (`other`). Both rows now score at
  `production` without those libraries, so their remaining `other` checks drop by the alignment
  rule. A check met only by a preview package (Durable Task ScheduledTasks) is not credited.

## 8. Change log

| Date | Method | Change |
|---|---|---|
| 2026-10-14 | 1.0 | Initial 82 rows × 11 frameworks; 0.6.0 scored. |
| 2026-10-15 | 1.0 | 0.7.0 re-scored (blind researcher passes on core and platform); `targets`, `track`, `stance` added; weights defined. Live-reload agent proposal 12 kept at 15, migrations proposal 6 kept at 3, and track record kept at 8, because under §3.1 the evidence had not changed (example-only or unmeasured). Compile-loop kept at 38 until measured. |
| 2026-10-15 | 1.0 | v2 checklists added (reported alongside). |
| 2026-10-16 | 1.1 | v1 caps applied strictly: 25 id_effect scores on 17 rows lowered to the cap of their v2 level (owner decision L13). |
| 2026-10-16 | 2.0 | v2 primary (L13). Competitor levels aligned by one rule (§7.4, `alignment-2.0.json`: 60 competitor scores changed); stored scores are checklist scores, validated; targets re-based (§7.4); method 1.1 kept in `rows[].v1`. ADR notes added at 0.7.0 for `test.unit` and `dx.diagnostics`, whose checklist scores drop from 0.6.0 (removed helpers and `id-effect-diagnose`). |
| 2026-10-17 | 2.1 | 0.9.0 measured by blind re-score (§6; five assessors without targets, HEAD 9930f7f): E 67.2 (#3; planned 64.6), RT 70.8 (#2), red 21, leader 19; 24 rows up, none down. Later targets re-planned from the new base (§5.1). |
| 2026-10-17 | 2.1 | 0.9 decision D1 applied: the E1.3 generators did not start by the end of wave 2, so `dx.scaffold`'s 0.9.0 target (88) moves to 0.10.0. Planned 0.9.0 E 64.9 → 64.6 (#3). |
| 2026-10-18 | 2.1 | 0.10.0 measured by blind re-score (§6; five assessors without targets): E 75.5 (#2; planned 73.8), RT 78.4 (#1), red 12, leader 24; 17 rows up, none down. Later planned indices re-based on the new measured scores (§5.1). |
| 2026-10-17 | 2.1 | 0.10 decision D1 applied: E1.9 (the dylint port of the lint crate) did not start by the end of wave 2, so `dx.lint`'s 0.10.0 target (63) moves to 0.11.0. Planned 0.10.0 E 74.1 → 73.8 (#2), red 11 → 12. |
| 2026-10-16 | 2.1 | 0.8.0 measured by blind re-score (§6; five assessors without targets): E 55.2 (#5), RT 64.4 (#2), red 31. One miss carried over (`data.jsonschema` 38 vs 50). The method 1.1 history may stop before the latest measured version (v1.id_effect is a subset). |
| 2026-10-16 | 2.1 | Process-row level rule (§7.4; owner decision D2 at 0.8 kickoff): 7 rows marked `process`; id_effect `eco.stability` 49 → 50 at 0.6.0 and 0.7.0 (no longer capped at `published`). `data.jsonschema` 0.8.0 target 75 → 50 (owner decision D1: OpenAPI from routes stays in 0.9). |
| 2026-10-18 | 2.2 | Non-goals leave the score (§3.3; L15, PLAN-OF-PLANS §7.0b step 1): row `web.graphql` (L15) and checks `fault.reload#2 #6 #7`, `dx.compile#3` (L8), `web.ui#3 #4` (L7), `data.backends#4 #5` (L6), for every framework. Rule recorded: a check id_effect already meets is never a non-goal, so `dx.repl#3` stays scored and `dx.repl` stays 75 (0.11.0.md D3). Targets of the four rows with non-goal checks re-based to the same number of checks (`alignment-2.2.json → targets_rebased`); `web.graphql` targets removed. Tooling: `non_goal {decision}` in scorecard.json and the schema, `--validate` rejects a non-goal without a locked-decision id or on a check id_effect meets, fixtures `non-goal-row.json`, `non-goal-check.json`. |
| 2026-10-18 | 2.2 | Scope change, competitor snapshot date unchanged (2026-10-14): the .NET scope adds Orleans and the Durable Task Framework as first-party Microsoft (L15, §7.0b step 2; §7.4). .NET `fault.reload` 80 (after step 1) → 100, `msg.jobs` 39 → 75, `msg.cron` 39 → 50; .NET U/E/RT 75.1/77.3/71.1 (2.1) → 75.8/77.9/71.8 (step 1) → 76.7/78.8/72.8. Evidence per cell in `alignment-2.2.json`. |
| 2026-10-18 | 2.2 | Every stored version re-scored (§5.2; 2.1 kept as frozen history): 0.10.0 measured E 75.5 → 76.6 (#2), RT 78.4 → 79.5 (#1), red 12 → 11. Targets: `web.rpc` and `web.realtime` 0.11.0 → 0.12.0 (L15, 0.11.0.md D4); 0.11.0 bonus targets `conc.stm` 89, `sec.supply` 100, `test.qa` 100 (0.11.0.md D1). Planned 0.11.0 E 79.2 (#1, .NET 78.8). Owner acceptance pending (PLAN-OF-PLANS §11, §7.0b step 4). |
| 2026-10-19 | 2.2 | 0.11.0 measured by blind re-score (§6; scored from the checks and the tree at release/0.11.0 834d9359, not from the targets): U 79.4, E 79.2 (#1; accepted 2.2 plan 79.2, tool-planned 79.5), RT 83.9 (#1), ≥80 56, red 9, leader 31; 11 rows up, none down. Adjudicated (§6 step 4, more than 10 points from the plan): `dx.lint` 75 vs 63, because #5 (MachineApplicable auto-fixes, `cargo dylint --fix`) is met as well as the planned #3 #4; `core.ergonomics` 88 vs 75 (no 0.11 target), because the published Dylint lints run over the workspace in CI (#7); both kept. One miss: `sec.supply` 88 vs 100, because #7 needs a published advisory history and no GHSA is published yet (SECURITY.md "Security releases" lists 0.10.1 with every advisory "pending"); carried over to 0.12.0 (new target 100). Evidence line references refreshed across the file (89 moved symbols). Later planned indices re-based on the new measured scores (§5.1). |
| 2026-10-20 | 2.2 | 0.12.0 measured by blind re-score (§6; a fresh assessor given this document, the 0.11.0 evidence and the tree at the release/0.12.0 candidate, not the targets): U 82.3, E 81.5 (#1; release plan 80.9, tool-planned 81.7), RT 85.9 (#1; plan 84.8), ≥80 59, red 7 (plan 7), leader 37; 7 rows up, none down, and `dist.remote`, `dist.placement`, `data.events` documented → production at the same score. Adjudicated (§6 step 4, more than 10 points from the plan or without a 0.12 target): `web.realtime` 100 vs 88 and `dist.tls` 100 vs ≥ 88 (the Relay runs both: production), `web.server` 100 (#6 TLS and HTTP/2 in the host), `data.jsonschema` 100 (#8 `id-effect rpc gen ts`), `conc.perf` 88 (#6 per-stream memory, #8 published p99), `fault.actors` 100 (the channels hub is a process in the Relay); all kept. One miss: `sec.supply` 88 vs 100, no published GHSA yet; carried over to 0.13.0 (target 100). The level audit's per-check evidence for `web.rpc`, `web.realtime`, `dist.tls` re-checked at HEAD; 35 evidence line references refreshed. |
| 2026-10-25 | 2.2 | 0.13.0 measured by blind re-score (§6; an assessor who built none of the 0.13 leaves, given this document, the 0.12.0 evidence, the r13-w8-level-audit evidence map and the tree at release/0.13.0 182d65a6): U 86.7, E 86.8 (#1; release plan 85.2 restated by ADR 0015, gate 84.2; tool-planned 87.1), RT 88.7 (#1; plan 87.8), ≥80 64, red 3 (plan ≤ 3), leader 40; 8 rows up, none down. Adjudicated (§6 step 4, more than 10 points from the plan or without a 0.13 target): `web.ui` 83 vs 67 (#5 form errors bound to fields met; #7 Leptos hydration not counted, examples-only), `web.admin` 88 vs 50 (#4 #5 #6 met), `ai.llm` 100 vs 75 (#4 #8 met), `eco.tenancy` 88 vs 75 (#8 `TenantRouter`), `web.validation` 100 (#8 translated Schema messages, id_effect_i18n) and `sec.web` 100 (#8 auto-escaping first-party views) without a 0.13 target; all kept. One miss: `sec.supply` 88 vs 100, still no published GHSA (0 on 2026-10-25); carried over to 0.14 in REPORT-0.13.0 and PLAN-OF-PLANS §11 (stored targets unchanged, 0.13 D13). Moved evidence (`id_effect_sql/src/repository.rs` → `repository/mod.rs`) refreshed for `data.orm`, `eco.tenancy`, `sec.authz`. |
