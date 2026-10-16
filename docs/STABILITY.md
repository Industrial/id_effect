# Stability policy

What id_effect promises about its public API, its deprecations and its minimum supported Rust
version (MSRV). The release plan behind it is PLAN-OF-PLANS decision L9
([`.maestro/specs/roadmap/PLAN-OF-PLANS.md`](../.maestro/specs/roadmap/PLAN-OF-PLANS.md)).

## Scope

The policy covers the public API of the crates published from `crates/` (the publish order is in
[`scripts/validate-crate-packaging.sh`](../scripts/validate-crate-packaging.sh)). It does not
cover:

- the unpublished `labs/` workspace (no API guarantees; [ADR 0010](adrs/0010-crate-tiers.md)),
- `examples/`,
- items marked `#[doc(hidden)]`, and anything documented as unstable or internal.

## Semver before 1.0 (now)

Cargo treats `0.x.y → 0.x.z` as compatible and `0.x → 0.(x+1)` as breaking. We follow that:

- **Patch releases** (`0.8.0 → 0.8.1`) never break the public API. They carry fixes, additions
  that cannot break callers, and security fixes.
- **Minor releases** (`0.7 → 0.8`) may break the public API, under the deprecation rule below.
- Every break is listed in that release's migration guide (`docs/MIGRATION-0.N.md`) and summarised
  in the [CHANGELOG](../CHANGELOG.md).

## Semver from 1.0

Strict [SemVer](https://semver.org/): breaking changes only in a major release, additions in
minors, fixes in patches. Deprecated items are removed only in the next major. The long-term
support (LTS) policy for 1.x is defined at 1.0.

## Deprecation rule

A breaking change to a public item ships in two steps:

1. **Minor N:** the new API lands. The old item stays as a `#[deprecated(since = "0.N.0", note =
   "use … instead")]` shim that forwards to the new one, and the migration guide for 0.N has an
   entry ("if you used X, now do Y").
2. **Minor N+1:** the shim may be removed.

When a shim is impossible (for example a trait gains a required method, or a type changes shape),
the migration guide entry says so explicitly and gives the replacement code. Reviewers check this
rule ([docs/QUALITY_BAR.md](QUALITY_BAR.md), judgement checks).

## Semver checks in CI

The `semver` job in [`.github/workflows/ci.yml`](../.github/workflows/ci.yml) runs
`cargo semver-checks` for the publishable crates against the previous release on crates.io (the
baseline is 0.7.0 during the 0.8 cycle). Locally: `moon run effect:semver`.

- **Until v0.8.0 is tagged** the job is report-only (`continue-on-error`): 0.8 carries intentional
  breaks. At the 0.8 exit gate every reported break must appear in
  [MIGRATION-0.8.md](MIGRATION-0.8.md); anything not listed is a bug to fix before tagging.
- **After v0.8.0** the baseline moves to 0.8.0 and the job enforces: an unlisted break fails CI.

## MSRV policy

- The MSRV is `rust-version` in the workspace [`Cargo.toml`](../Cargo.toml) (currently **1.94**)
  and is verified by the `msrv` CI job, which builds the workspace with exactly that toolchain.
- The MSRV is **at most N-2 stable**: with stable Rust at 1.N, the MSRV is never newer than
  1.(N-2), so the three most recent stable releases always build id_effect. (As of 0.8 the CI
  toolchain is stable 1.99.0 and the MSRV is 1.94, inside the bound.)
- Raising the MSRV is allowed in a minor release (not a patch release), only as far as the N-2
  bound, and is noted in the CHANGELOG. It is not treated as a semver break before 1.0.
