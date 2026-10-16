# Governance

How decisions are made in id_effect, who makes them, and how that is meant to change before 1.0.

## Maintainers

| Name | GitHub | Role |
|---|---|---|
| Tom Wieland | [@Industrial](https://github.com/Industrial) | Lead maintainer: merge rights, releases, security contact |

id_effect currently has **one** maintainer with merge rights. That is a bus-factor risk and we
say so plainly; the plan is at least three maintainers with merge rights by 1.0 (PLAN-OF-PLANS
epic E0.4). Code ownership is recorded in [`.github/CODEOWNERS`](.github/CODEOWNERS).

## Decision process

- **Day-to-day changes** (fixes, docs, tests, additions inside an existing crate's scope) are
  decided in the pull request. A maintainer reviews and merges once CI (`CI Pass`) is green and
  the change meets [docs/QUALITY_BAR.md](docs/QUALITY_BAR.md).
- **Significant changes** need an Architecture Decision Record first (see below): a new crate or
  tier change, a new locked dependency, a change to the runtime model, a public-API break outside
  the [deprecation rule](docs/STABILITY.md#deprecation-rule), or anything that contradicts an
  accepted ADR.
- Disagreements are discussed in the issue or PR. Maintainers aim for consensus; while there is a
  single maintainer, the lead maintainer decides and records the reasoning in the PR or ADR. With
  more maintainers, an unresolved disagreement is decided by the lead maintainer after every
  maintainer has stated a position in writing.

## ADR process

Architecture decisions live in [`docs/adrs/`](docs/adrs/) as `NNNN-short-title.md`, numbered
sequentially.

1. Open a PR adding the ADR with **Status: Proposed** and the sections used by the existing ADRs:
   Status, Context, Decision, Consequences, Alternatives considered.
2. Discussion happens on the PR. The ADR is merged with **Status: Accepted (date)** once a
   maintainer approves it, or closed as rejected.
3. Accepted ADRs are not rewritten. A later decision **amends** an ADR (a dated "Amendment"
   section) or **supersedes** it (a new ADR, with both Status sections cross-linked), as
   [ADR 0012](docs/adrs/0012-platform-vision.md) does for [ADR 0010](docs/adrs/0010-crate-tiers.md).

The locked roadmap decisions in PLAN-OF-PLANS §3.1 change only by ADR.

## Becoming a co-maintainer

Merge rights are given to contributors who have shown, over time, that they share the quality
bar. The path:

1. **Contributor:** a track record of merged pull requests (as a guide: at least five
   non-trivial ones over at least two months), including tests and docs that meet the quality bar.
2. **Reviewer:** the maintainers invite you to review pull requests in an area you know. Your
   reviews are advisory.
3. **Co-maintainer:** after a period of good reviews, any maintainer may nominate you. With no
   objection from the existing maintainers within 14 days, you get merge rights on the
   repository and are added to the table above and to `.github/CODEOWNERS` for your area.

Maintainers who become inactive for six months move to an emeritus list and their merge rights
are removed; they can return through the same nomination step.

## Releases

Releases are cut against the roadmap in
[`.maestro/specs/roadmap/PLAN-OF-PLANS.md`](.maestro/specs/roadmap/PLAN-OF-PLANS.md):

1. **Kickoff (§7.1):** the release's slot in the release train (§6) defines the epics in scope
   and the scorecard targets; a release spec is written under `.maestro/specs/releases/`.
2. **During the release (§7.2):** each change records its evidence; breaking changes follow
   [docs/STABILITY.md](docs/STABILITY.md) and are listed in the release's migration guide.
3. **Exit gate (§7.3):** the quality bar, semver checks, bench gates, scorecard re-score and
   reports must pass before tagging.
4. **Tag and publish:** the lead maintainer bumps the workspace version, pushes a `vX.Y.Z` tag,
   and [`.github/workflows/publish.yml`](.github/workflows/publish.yml) verifies versions, tests,
   publishes the crates to crates.io in dependency order, and creates the GitHub release with
   SBOMs and build provenance.
5. **Retro (§7.5):** the plan is updated with what slipped.

Security fixes may be released outside this cycle as patch releases ([SECURITY.md](SECURITY.md)).
