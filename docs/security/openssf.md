# OpenSSF Scorecard and Best Practices

id_effect measures its supply-chain posture with the
[OpenSSF Scorecard](https://scorecard.dev) and applies for the
[OpenSSF Best Practices](https://www.bestpractices.dev) badge (0.11.0 decisions D14, D15, D16).
The 0.11.0 target is a Scorecard score of at least 7.5 and the Best Practices "passing" level.

This page records:

- what each Scorecard check measures here, the score we expect at the 0.11.0 exit, the evidence,
  and what it still needs (a leaf of 0.11.0 or an owner action);
- the drafted Best Practices answers, for the owner to submit on bestpractices.dev (D16).

Some of the results depend on repository settings that only the owner can change, so the code in
this repository cannot reach 7.5 on its own.

## Workflows

| Workflow | What it does | Triggers |
|---|---|---|
| [`.github/workflows/codeql.yml`](../../.github/workflows/codeql.yml) | CodeQL for Rust and for GitHub Actions (`build-mode: none`), SARIF uploaded to code scanning | push and pull request to `main` and `release/**`, weekly (Monday 05:00 UTC), manual |
| [`.github/workflows/scorecard.yml`](../../.github/workflows/scorecard.yml) | `ossf/scorecard-action` with `publish_results: true` (feeds the README badge and the [viewer](https://scorecard.dev/viewer/?uri=github.com/Industrial/id_effect)), SARIF uploaded to code scanning | push to `main`, weekly (Monday 04:30 UTC), branch protection changes, manual |

Both pin every action by full commit SHA with the version in a trailing comment, set top-level
`permissions: contents: read`, and widen permissions per job only: `security-events: write` for
the SARIF upload, plus `id-token: write` (signs the published Scorecard results) and
`actions: read` for Scorecard. CodeQL is the only SAST tool; Semgrep is not added (D14).

To measure the score yourself (the CLI needs a GitHub token, read-only scopes are enough):

```bash
GITHUB_AUTH_TOKEN=$(gh auth token) scorecard --repo=github.com/Industrial/id_effect --show-details
# file-based checks only, against a local checkout:
GITHUB_AUTH_TOKEN=$(gh auth token) scorecard --local=. --checks=Pinned-Dependencies,Token-Permissions,SAST,Security-Policy
```

## Scorecard

### Baseline

Measured on 2026-10-18 with Scorecard v5.6.0 against `main` at `05808e56` (0.10.0):
**5.0**. The aggregate is the risk-weighted mean of the checks that return a score (Critical 10,
High 7.5, Medium 5, Low 2.5; inconclusive checks, score -1, are left out). Recomputing it from the
per-check scores with those weights gives 5.03, so the projections below use the same formula.

### Per check

"Exit" is the expected score when 0.11.0 is tagged and the code leaves of E11.3 and E11.4 have
merged. "Needs" names what moves the score; an owner action is something only a repository admin
can do.

| Check | Risk | Baseline | Exit | Evidence | Needs |
|---|---|---|---|---|---|
| Binary-Artifacts | High | 10 | 10 | No binaries in the repository. | Nothing. Keep binaries (fuzz corpora, built tools) out of git. |
| Branch-Protection | High | 4 | 4, or 8 with the owner action | `main` disallows deletion and force pushes, requires PRs, status checks and up-to-date branches, and applies to administrators. Missing: required approvals, approval of the last push, code-owner review, stale-review dismissal. | **Owner action (D15).** On `main` and `release/*`: require 1 approval and approval of the most recent push (Tier 2, score 6 to 8 with the status checks already in place); code-owner review adds Tier 4. With one maintainer a required approval blocks self-merges, so it needs either a second reviewer (GOVERNANCE.md, E0.4) or a ruleset bypass for the maintainer, which costs the "applies to administrators" point. |
| CI-Tests | Low | 10 | 10 | 30 of the last 30 merged PRs ran CI ([`.github/workflows/ci.yml`](../../.github/workflows/ci.yml)). | Nothing. Keep merging through PRs. |
| CII-Best-Practices | Low | 0 | 0, or 5 when Passing | No badge yet. Passing scores 5, Silver 7, Gold 10. | **Owner action (D16):** submit the answers below. |
| Code-Review | High | 0 | 0 | 1 of 13 changesets approved by someone other than the author: one maintainer (GOVERNANCE.md). | A second reviewer (E0.4). Partial credit: Dependabot PRs approved by hand count as reviewed changesets once r11-sc-pin-permissions removes the bot self-approval in `dependabot-auto-merge.yml`. Not counted in the projection. |
| Contributors | Low | 3 | 3 | Contributors from 1 organisation. | Contributors from more organisations; not under the project's control. |
| Dangerous-Workflow | Critical | 10 | 10 | No `pull_request_target` or `workflow_run` checkout of untrusted code, no script injection. | Nothing. |
| Dependency-Update-Tool | High | 10 | 10 | [`.github/dependabot.yml`](../../.github/dependabot.yml) (Cargo and Actions, weekly). | Nothing. |
| Fuzzing | Medium | 0 | 10 | ClusterFuzzLite and nine cargo-fuzz targets ([`docs/security/fuzzing.md`](fuzzing.md), `.clusterfuzzlite/`, `fuzz/fuzz_targets/`); measured 10 with `scorecard --local` on the 0.11 tree. | r11-fuzz-ci merged to `main`. |
| License | Low | 10 | 10 | [`LICENSE`](../../LICENSE), MIT (OSI). | Nothing. |
| Maintained | High | 10 | 10 | 19 commits in the last 90 days. | Keep at least one commit a week. |
| Packaging | Medium | -1 | -1 | Scorecard does not recognise `cargo publish` as a packaging workflow, so the check is inconclusive and left out of the aggregate. | Nothing (no effect on the score). |
| Pinned-Dependencies | Medium | 0 | 10 | Actions pinned by tag in `ci.yml`, `audit.yml`, `docs.yml`, `publish.yml`; the new `codeql.yml` and `scorecard.yml` and the ClusterFuzzLite workflows and Dockerfile are pinned (measured 10 on the two new workflows alone). Scorecard also cannot parse `.github/scripts/publish-crate.sh` ("reached EOF without closing quote"), so that file is not analysed. | r11-sc-pin-permissions and r11-sc-publish. |
| SAST | Medium | 0 | 7, then 10 | `codeql.yml` (CodeQL configured: Scorecard weighs this 7 in 10). The other 3 come from the share of the last 30 merged PRs that ran a SAST check: 0 at the exit, 30 of 30 after 30 PRs. | This leaf; time. |
| Security-Policy | Medium | 3 | 3, or 10 with the fix | [`SECURITY.md`](../../SECURITY.md) alone scores 10 (measured). The repository scores 3 because Scorecard also matches `.codex/skills/maestro-setup/reference/context-templates/security.md`, a template without links or a disclosure process, and scores that file (reproduced with both files in a scratch directory). | Move or rename that template so no file other than `SECURITY.md` is named `security.md` (outside this leaf: harness files). |
| Signed-Releases | High | 0 | 2, then 10 | The last releases (v0.8.0, v0.9.0, v0.10.0) attach `.crate` files and SBOMs but no signature or provenance file: GitHub artifact attestations are stored outside the release, so Scorecard does not see them. | r11-sc-publish attaches the SLSA provenance (`*.intoto.jsonl`) to the GitHub release. Scorecard averages the last 5 releases with assets (provenance 10, signature 8, none 0), so 0.11.0 alone gives about 2; 10 after five such releases. **Owner action (D15):** signed tags for the release check. |
| Token-Permissions | High | 0 | 10 | Missing top-level `permissions:` in `ci.yml`, `audit.yml`, `docs.yml`, `publish.yml`; top-level `contents: write` in `dependabot-auto-merge.yml`. Job-level writes do not lower the score (measured: a job-level `contents: write` release job still scores 10). | r11-sc-pin-permissions and r11-sc-publish. **Owner action (D15):** delete the `CARGO_REGISTRY_TOKEN` secret after the first trusted publish (not scored by this check, but part of the same finding, S3). |
| Vulnerabilities | High | 10 | 9 to 10 | No OSV vulnerability on `main` (no committed `Cargo.lock` there). From 0.11 `Cargo.lock` is committed (D12) and contains `rsa` 0.9.10 (RUSTSEC-2023-0071, no fixed version; accepted with a reachability reason in [`deny.toml`](../../deny.toml)); `scorecard --local` on the 0.11 lockfile reported 0, so 9 is the cautious figure. | Nothing in this release; THREAT-MODEL.md (r11-sc-dco-docs) records the accepted risk. |

Webhooks needs an admin token and is not run by the action; SBOM is not part of the v5.6.0
aggregate. Neither counts.

### Projection

Computed with the weights above (High 7.5, Critical 10, Medium 5, Low 2.5), the exit column, and
the cautious Vulnerabilities 9:

| Scenario | Score |
|---|---|
| A. Exit, code leaves merged, no owner action | 7.2 |
| B. A + the Security-Policy template moved | 7.55 |
| C. B + Best Practices Passing (D16) | 7.67 |
| D. C + required approvals on `main` and `release/*` (D15) | 7.97 |
| A + D15 + D16, template not moved | 7.62 |
| Steady state: D after 30 PRs with CodeQL and five releases with provenance | 8.72 |

7.5 needs the code leaves (Pinned-Dependencies, Token-Permissions, Fuzzing, SAST) plus at least one
of: the Security-Policy fix, or both owner actions. Code-Review stays at 0 with one maintainer;
it is the largest single shortfall (High weight). The exit gate reads the actual score from the
Scorecard workflow run (or `scorecard --repo=github.com/Industrial/id_effect`) and records it in
the release report.

### Owner actions

1. **D15, branch protection:** on `main` and `release/*`, require 1 approval and approval of the
   most recent push, keep the required status checks; optionally require code-owner review.
2. **D15, releases and tokens:** "require signed commits", signed release tags, the crates.io
   trusted publisher for `Industrial/id_effect` `publish.yml`, then delete `CARGO_REGISTRY_TOKEN`.
3. **D16:** create the project on bestpractices.dev, enter the answers below, and add the badge to
   the README badge row 3 once it shows Passing:
   `[![OpenSSF Best Practices](https://www.bestpractices.dev/projects/<id>/badge)](https://www.bestpractices.dev/projects/<id>)`.
4. **Code scanning:** triage the first CodeQL alerts (see "First CodeQL run" below); the 0.11.0
   exit needs no open high or critical alert.

## First CodeQL run

Run locally on 2026-10-18 with the CodeQL bundle v2.27.2 and the same default query suites the
workflow uses (`rust-code-scanning.qls`, `actions-code-scanning.qls`), over the 0.11 tree before
r11-sc-pin-permissions and r11-sc-publish merged.

**Rust: 26 alerts, all `rust/hard-coded-cryptographic-value` (critical), all test or example
fixtures.** 23 are string literals inside `#[cfg(test)] mod tests`
(`crates/id_effect_auth/src/password.rs`, `basic.rs`, `crates/id_effect_mail/src/smtp.rs`,
`crates/id_effect_node/src/handshake.rs`, `crates/id_effect_test/src/containers/mysql.rs`);
3 are the demo passwords in `crates/id_effect_auth/examples/005_passwords_sessions.rs`. None is in
production code. The workflow keeps the query enabled, because the same query guards the
production paths; the owner dismisses these alerts in code scanning as "Used in tests" after the
first run on GitHub.

**Actions: 20 alerts, all `actions/missing-workflow-permissions` (medium)** in `ci.yml`,
`audit.yml`, `docs.yml` and `publish.yml`: the missing top-level `permissions:` that
r11-sc-pin-permissions and r11-sc-publish add (findings S1, S2). None in `codeql.yml`,
`scorecard.yml` or the ClusterFuzzLite workflows. No high or critical Actions alert.

## OpenSSF Best Practices: Passing answers (draft)

Drafted from what the repository contains at 0.11.0; each answer cites the evidence to paste in
the questionnaire's justification field. Status is **Met**, **Unmet** or **N/A** as the
questionnaire uses them. "Attest" marks an answer that is the owner's own statement: the
repository supports it but cannot prove it. Repository URL: `https://github.com/Industrial/id_effect`;
project site: `https://industrial.github.io/id_effect/`.

### Basics

| Criterion | Level | Status | Evidence |
|---|---|---|---|
| `description_good` | MUST | Met | [`README.md`](../../README.md) first paragraph: what id_effect is and does. |
| `interact` | MUST | Met | README (crates.io and docs.rs badges, the book), GitHub Issues and PRs, [`CONTRIBUTING.md`](../../CONTRIBUTING.md). |
| `contribution` | MUST | Met | [`CONTRIBUTING.md`](../../CONTRIBUTING.md) (licence of contributions, DCO), [`GOVERNANCE.md`](../../GOVERNANCE.md) (how PRs are reviewed and merged). |
| `contribution_requirements` | SHOULD | Met | CONTRIBUTING.md links the gates every change must pass: [`docs/QUALITY_BAR.md`](../QUALITY_BAR.md); style in [`TESTING.md`](../../TESTING.md), `rustfmt.toml`, `treefmt.toml`. |
| `floss_license` | MUST | Met | MIT, [`LICENSE`](../../LICENSE). |
| `floss_license_osi` | SUGGESTED | Met | MIT is OSI-approved. |
| `license_location` | MUST | Met | `LICENSE` at the repository root. |
| `documentation_basics` | MUST | Met | The book "Typed Effects in Rust" (`crates/id_effect/book/`, published at `https://industrial.github.io/id_effect/` by `.github/workflows/docs.yml`), crate READMEs, `docs/`. |
| `documentation_interface` | MUST | Met | API reference on docs.rs; `#![deny(missing_docs)]` is required in the crate roots of the core, runtime, integration and platform tiers (QUALITY_BAR.md check D). |
| `sites_https` | MUST | Met | github.com, industrial.github.io, crates.io and docs.rs are HTTPS only. |
| `discussion` | MUST | Met | GitHub Issues and pull requests: searchable, addressable by URL, open to new participants. |
| `english` | SHOULD | Met | All documentation and the issue tracker are in English. |
| `maintained` | MUST | Met | Releases 0.6.0 to 0.10.0 in October 2026 (CHANGELOG.md); weekly Dependabot updates. |

### Change control

| Criterion | Level | Status | Evidence |
|---|---|---|---|
| `repo_public` | MUST | Met | `https://github.com/Industrial/id_effect` (public). |
| `repo_track` | MUST | Met | git: every change with author and date. |
| `repo_interim` | MUST | Met | Work between releases lands on `main` and `release/*` branches through PRs. |
| `repo_distributed` | SUGGESTED | Met | git. |
| `version_unique` | MUST | Met | One workspace version (`Cargo.toml` `[workspace.package]`), one `vX.Y.Z` tag per release ([`docs/STABILITY.md`](../STABILITY.md)). |
| `version_semver` | SUGGESTED | Met | Semantic versioning with the pre-1.0 rules in [`docs/STABILITY.md`](../STABILITY.md). |
| `version_tags` | SUGGESTED | Met | Tags `v0.2.0` to `v0.10.0`; publishing runs from a `v*` tag ([`.github/workflows/publish.yml`](../../.github/workflows/publish.yml)). |
| `release_notes` | MUST | Met | [`CHANGELOG.md`](../../CHANGELOG.md), one section per release, plus `docs/MIGRATION-*.md` per breaking release; GitHub releases. |
| `release_notes_vulns` | MUST | N/A | No release so far fixed a vulnerability with a CVE or GHSA (0 repository advisories). The policy that the CHANGELOG names each advisory is in [`SECURITY.md`](../../SECURITY.md) ("Advisory publication", step 5), and security fixes are listed under "Security" (CHANGELOG.md, 0.10.0). |

### Reporting

| Criterion | Level | Status | Evidence |
|---|---|---|---|
| `report_process` | MUST | Met | GitHub Issues are enabled: `https://github.com/Industrial/id_effect/issues`. No document says "report bugs in GitHub Issues" yet; adding that line to CONTRIBUTING.md makes this answer stronger. |
| `report_tracker` | SHOULD | Met | GitHub Issues. |
| `report_responses` | MUST | Met (Attest) | The bug reports in the last 12 months (#42 to #56, April 2026, one reporter) are all closed, #42 with replies. The owner confirms each was answered. |
| `enhancement_responses` | SHOULD | Met (Attest) | As above. |
| `report_archive` | MUST | Met | Issues and PRs stay public and searchable on GitHub. |
| `vulnerability_report_process` | MUST | Met | [`SECURITY.md`](../../SECURITY.md) "Reporting a vulnerability". |
| `vulnerability_report_private` | MUST | Met | GitHub private vulnerability reporting is enabled (repository setting, checked 2026-10-18) and linked from SECURITY.md. |
| `vulnerability_report_response` | MUST | N/A | No vulnerability report in the last 6 months (0 repository advisories). The targets (acknowledge within 7 days, assess within 14) are in SECURITY.md "Response times". |

### Quality

| Criterion | Level | Status | Evidence |
|---|---|---|---|
| `build` | MUST | Met | `cargo build --workspace` (Cargo workspace, `Cargo.toml`). |
| `build_common_tools` | SUGGESTED | Met | Cargo; moon and treefmt for the repository tasks ([`moon.yml`](../../moon.yml)). |
| `build_floss_tools` | SHOULD | Met | Rust toolchain, Cargo, moon, treefmt, Nix: all FLOSS. |
| `test` | MUST | Met | `cargo nextest run --workspace` / `cargo test --workspace`; strategy in [`TESTING.md`](../../TESTING.md); unit tests in-crate, integration tests under `crates/*/tests/`, `tests/`. |
| `test_invocation` | SHOULD | Met | The standard `cargo test` works; CI uses `cargo nextest run`. |
| `test_most` | SUGGESTED | Met | Coverage gate ≥ 96 % lines, regions and functions on published crates ([`docs/QUALITY_BAR.md`](../QUALITY_BAR.md), `scripts/llvm-cov-nextest.sh`, `coverage` job in `ci.yml`, Codecov badge). |
| `test_continuous_integration` | SUGGESTED | Met | [`.github/workflows/ci.yml`](../../.github/workflows/ci.yml) on every push and PR (moon CI, coverage, OS and toolchain matrix, MSRV, Postgres and container integration, minimal versions). |
| `test_policy` | MUST | Met | [`TESTING.md`](../../TESTING.md) (tests written before or with the implementation), QUALITY_BAR.md (test density check N, coverage ≥ 96 %), linked from CONTRIBUTING.md as the gate every change passes. |
| `tests_are_added` | MUST | Met | Recent changes add tests with the code, e.g. the security fixes (`crates/id_effect_node/tests/mesh_security.rs`, `crates/id_effect_axum/tests/security.rs`), the fuzz targets (`fuzz/fuzz_targets/`), `tests/security_compile_fail/`; the release spec names a test per finding. |
| `tests_documented_added` | SUGGESTED | Met | TESTING.md and QUALITY_BAR.md (linked from CONTRIBUTING.md). |
| `warnings` | MUST | Met | `cargo clippy --workspace --all-targets -- -D warnings` (QUALITY_BAR.md "Workspace checks", moon `clippy` task run by CI). |
| `warnings_fixed` | MUST | Met | `-D warnings` fails CI on any warning. |
| `warnings_strict` | SUGGESTED | Met | Warnings are errors; `#![deny(missing_docs)]` and `#![forbid(unsafe_code)]` in the crate roots. |

### Security

| Criterion | Level | Status | Evidence |
|---|---|---|---|
| `know_secure_design` | MUST | Met (Attest) | The owner states it. Supporting evidence: THREAT-MODEL.md, ADR 0013 (`docs/adrs/`), secure defaults with explicit `dangerous_*` opt-outs. |
| `know_common_errors` | MUST | Met (Attest) | The owner states it. Supporting evidence: the security findings register and their fixes (0.11.0 release spec, CHANGELOG.md), fuzzing of every untrusted-input decoder ([`fuzzing.md`](fuzzing.md)), CodeQL. |
| `crypto_published` | MUST | Met | Only published algorithms: Argon2id for passwords (`crates/id_effect_auth/src/password.rs`), HMAC-SHA-256, JWT with the JOSE algorithms (`jwt.rs`), TOTP per RFC 6238 (`totp.rs`), TLS 1.2/1.3 via rustls. |
| `crypto_call` | SHOULD | Met | No own primitives: `argon2`, `jsonwebtoken`, `totp-rs`, `hmac`, `sha2`, `subtle`, `rustls`, `aws-lc-rs`, `cookie` (workspace `Cargo.toml`). |
| `crypto_floss` | MUST | Met | All of the above are FLOSS (licences checked by `cargo deny check`, [`deny.toml`](../../deny.toml)). |
| `crypto_keylength` | MUST | Met | Generated secrets are 256 bits (API keys `apikey.rs:37`, session ids `session/mod.rs:60`, account tokens `account/token.rs:17`); the QUIC self-signed certificate uses rcgen's default ECDSA P-256 key (`crates/id_effect_node/src/transport/quic.rs:51`). Keys the user supplies (HS256 secrets, RSA JWKs) are the user's; the HS256 docs ask for at least 32 random bytes (`jwt.rs:631`). |
| `crypto_working` | MUST | Met | No MD4, MD5, single DES, RC4 or Dual_EC_DRBG in the sources. |
| `crypto_weaknesses` | SHOULD | Unmet | TOTP defaults to HMAC-SHA-1 (`crates/id_effect_auth/src/totp.rs:178`), the RFC 6238 default that authenticator apps expect. HMAC-SHA-1 is not affected by the SHA-1 collision attacks, but the criterion names SHA-1. Justification text: "HMAC-SHA-1 for TOTP only, for authenticator-app compatibility; SHA-256 and SHA-512 are selectable." |
| `crypto_pfs` | SHOULD | Met | TLS through rustls, which only offers (EC)DHE key exchange. |
| `crypto_password_storage` | MUST | Met | Argon2id PHC strings with the OWASP minimum parameters and a fresh random salt per hash (`crates/id_effect_auth/src/password.rs`, test `hash_is_a_salted_argon2id_phc_string`). |
| `crypto_random` | MUST | Met | Keys, ids and nonces come from the operating system CSPRNG (`getrandom` / `rand`; `apikey.rs`, `session/mod.rs`, `totp.rs`, `account/token.rs`). |
| `delivery_mitm` | MUST | Met | Delivered through crates.io over HTTPS with Cargo's checksums; source on GitHub over HTTPS/SSH; SLSA provenance on releases (r11-sc-publish). |
| `delivery_unsigned` | MUST | Met | No hash is fetched over HTTP; Cargo verifies the registry checksum. |
| `vulnerabilities_fixed_60_days` | MUST | Met | No unpatched publicly known vulnerability in id_effect. In a dependency: `rsa` 0.9 (RUSTSEC-2023-0071) has no fixed release; it is not reachable from id_effect's code paths (reason in `deny.toml`, accepted risk in THREAT-MODEL.md). |
| `vulnerabilities_critical_fixed` | SHOULD | Met | No publicly known critical vulnerability has been reported; [`SECURITY.md`](../../SECURITY.md) commits to fixing high and critical issues sooner than its 90-day target. |
| `no_leaked_credentials` | MUST | Met (Attest) | No credentials are committed; secrets live in GitHub Actions secrets. The test and example configs hold only throwaway development values. No secret scanner was run for this draft: the owner checks GitHub secret scanning before submitting. |

### Analysis

| Criterion | Level | Status | Evidence |
|---|---|---|---|
| `static_analysis` | MUST | Met | Clippy with `-D warnings` on every change; CodeQL for Rust and Actions ([`.github/workflows/codeql.yml`](../../.github/workflows/codeql.yml)); `cargo deny` / `cargo audit` for dependencies. |
| `static_analysis_common_vulnerabilities` | SUGGESTED | Met | CodeQL's security queries for Rust and Actions. |
| `static_analysis_fixed` | MUST | Met at the 0.11.0 exit | The exit gate needs no open high or critical CodeQL alert (0.11.0 release spec, S4). The first local run found only test and example fixtures (Rust) and missing workflow permissions fixed by two 0.11 leaves (Actions); see "First CodeQL run". Answer once the run on GitHub is triaged. |
| `static_analysis_often` | SUGGESTED | Met | CodeQL on every push and PR and weekly; Clippy on every PR. |
| `dynamic_analysis` | SUGGESTED | Met | Nine cargo-fuzz targets, ClusterFuzzLite on PRs and nightly ([`fuzzing.md`](fuzzing.md), `.github/workflows/cflite_pr.yml`, `cflite_batch.yml`). |
| `dynamic_analysis_unsafe` | SUGGESTED | Met | Rust with no `unsafe` block in `crates/*/src` and `#![forbid(unsafe_code)]` in most crate roots; the fuzzers run under AddressSanitizer (`cflite_pr.yml`, `sanitizer: address`). |
| `dynamic_analysis_enable_assertions` | SUGGESTED | Met | The fuzz targets assert documented properties besides "no panic" and build with debug assertions (`cargo fuzz build -O --debug-assertions` in `.clusterfuzzlite/build.sh`). |
| `dynamic_analysis_fixed` | MUST | Met (Attest) | No open fuzzing crash at the time of drafting; a crash fails the PR job (`cflite_pr.yml`). The owner re-checks before submitting. |

### Unmet and open answers

- **Unmet:** `crypto_weaknesses` (SHOULD): TOTP defaults to HMAC-SHA-1. A SHOULD answered Unmet
  with a justification does not block Passing.
- **N/A:** `release_notes_vulns`, `vulnerability_report_response` (no advisory and no
  vulnerability report so far).
- **Answer after the first CodeQL run:** `static_analysis_fixed` (MUST).
- **Owner attestations:** `know_secure_design`, `know_common_errors`, `report_responses`,
  `enhancement_responses`, `no_leaked_credentials`, `dynamic_analysis_fixed`.
- **Stronger with one line of docs:** `report_process` (say in CONTRIBUTING.md that bugs and
  feature requests go to GitHub Issues).

No MUST criterion is Unmet, so the project qualifies for Passing once the CodeQL alerts are
triaged.
