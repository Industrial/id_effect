# Platform Scorecard — Method (v1.0)

This scorecard measures how well id_effect serves a serious corporate team, compared with ten
reference frameworks. Every release is re-scored against it. The plan of plans
(`../../.maestro/specs/roadmap/PLAN-OF-PLANS.md`) sets a target per row per release, and each
release proves its progress by re-running this method.

- **Data:** `scorecard.json` (single source of truth: rows, competitor scores, id_effect scores per
  version, evidence, tracks, targets).
- **Rendered view:** the published HTML matrix, `docs/scorecard/index.html` (produced from
  `scorecard.json` by the `leaf-scorecard-report` task).
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

"Out of the box" means first-party, or the de facto standard library that the framework's own docs
recommend. A third-party-only answer is scored about 5–10 points lower than the same capability
shipped first-party.

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

### 5.2 Reference results (method v1.0, competitor snapshot 2026-10-14)

| Version | U | E (rank) | RT (rank) | ≥80 | <40 | leader | median gap |
|---|---|---|---|---|---|---|---|
| 0.6.0 measured | 45.9 | 44.0 (#11) | 55.4 (#8) | 12 | 34 | 2 | 47 |
| 0.7.0 measured | 49.1 | 46.8 (#11) | 59.1 (#7) | 16 | 31 | 3 | 40 |
| 0.8.0 target | 55.8 | 54.3 (#11) | 64.3 (#7) | 25 | 24 | 4 | 28 |
| 0.9.0 target | 63.3 | 62.8 (#9) | 68.2 (#6) | 35 | 18 | 4 | 20 |
| 0.10.0 target | 69.6 | 69.4 (#5) | 74.1 (#4) | 41 | 11 | 6 | 15 |
| 0.11.0 target | 73.1 | 72.3 (#4) | 78.6 (#2) | 50 | 8 | 8 | 12 |
| 0.12.0 target | 76.7 | 76.5 (#4) | 80.9 (#1) | 51 | 3 | 8 | 12 |
| 1.0.0 target | 80.4 | 80.4 (#3) | 83.5 (#1) | 59 | 1 | 10 | 11 |

Competitor indices in the same snapshot:

| Framework | U | E | RT |
|---|---|---|---|
| .NET | 83.9 | 85.4 | 80.3 |
| Spring | 78.8 | 81.1 | 74.3 |
| Phoenix | 77.6 | 77.6 | 75.4 |
| Laravel | 69.1 | 71.3 | 57.5 |
| Effect-TS | 67.9 | 67.1 | 69.8 |
| Rails | 65.9 | 67.2 | 54.3 |
| ZIO | 65.7 | 63.8 | 68.7 |
| Django | 62.9 | 64.3 | 50.8 |
| OTP | 58.9 | 57.8 | 65.0 |
| Next.js | 56.9 | 58.7 | 49.2 |

Any implementation of §5 **must** reproduce this table exactly from `scorecard.json`. This table is
the tool's self-test fixture.

## 6. Re-test protocol (each release)

1. Check out the release tag in a clean worktree.
2. A **fresh, blind assessor** re-scores the id_effect column. This is a new agent session or a
   person who has not seen the targets, given only this document, the previous `evidence` strings
   and the repo. Per row they record a score and an evidence string.
3. Apply the §3.2 caps.
4. Adjudicate: any row whose blind score differs from the release owner's expectation by more than
   10 points is discussed, and the decision is recorded in the release report.
5. Write the scores under the new version key in `scorecard.json → rows[].id_effect`, then
   recompute §5.
6. Publish `docs/scorecard/results/<version>.json` and `REPORT-<version>.md`. The report has: the
   indices table, the deltas against the previous version and against the targets, the rows that
   changed with their evidence, any regressions, and any misses with their carry-over.

## 7. Method v2 (planned, `leaf-method-v2-checklists`)

v1 is anchored judgment plus hard caps. v2 makes it mechanical: each row gets 4–8 **table-stakes
checks**, phrased so a yes/no answer is objective (example for `sec.authn`: argon2 password hashing;
server-side sessions with a persistent store; JWT verify/issue; OIDC with PKCE; MFA/TOTP; passkeys).
The v2 score is:

```
s = clamp(1, cap(level), round(100 × checks_met / checks_total))
```

`cap(level)` is taken from §3.2. Competitor columns are checked against the same lists. Moving to v2
re-baselines every stored version, and reports show v1 and v2 side by side for one release.

## 8. Change log

| Date | Method | Change |
|---|---|---|
| 2026-10-14 | 1.0 | Initial 82 rows × 11 frameworks; 0.6.0 scored. |
| 2026-10-15 | 1.0 | 0.7.0 re-scored (blind researcher passes on core and platform); `targets`, `track`, `stance` added; weights defined. Live-reload agent proposal 12 kept at 15, migrations proposal 6 kept at 3, and track record kept at 8, because under §3.1 the evidence had not changed (example-only or unmeasured). Compile-loop kept at 38 until measured. |
